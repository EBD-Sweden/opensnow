//! SSO/OIDC authentication for OpenSnow.
//!
//! Supports multi-tenant OIDC: each tenant has its own IdP configuration.
//! Flow:
//!   1. Email domain → tenant lookup
//!   2. Tenant has sso_enabled=true, oidc_issuer, oidc_client_id, oidc_client_secret
//!   3. Exchange authorization code OR verify id_token
//!   4. Upsert user, sync roles from sso_role_mappings
//!   5. Return JWT for OpenSnow session

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use rand::{Rng, distributions::Alphanumeric};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::contract::{SecretHandleDescriptor, SecretPurpose, SecretType};
use crate::secrets::{ExternalSecretResolver, SecretProvider, TrustedSecretStore};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantSsoConfig {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub sso_enabled: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_client_secret: Option<String>,
    pub oidc_scopes: String,
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsoRoleMapping {
    pub id: String,
    pub tenant_id: String,
    pub idp_claim_key: String,
    pub idp_claim_value: String,
    pub role_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcClaims {
    pub sub: String,
    #[serde(default)]
    pub iss: Option<String>,
    #[serde(default)]
    pub aud: Option<serde_json::Value>,
    #[serde(default)]
    pub nonce: Option<String>,
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    pub name: Option<String>,
    pub picture: Option<String>,
    pub preferred_username: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsoLoginRequest {
    pub email: String,
    pub code: Option<String>,
    pub id_token: Option<String>,
    pub redirect_uri: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SsoProtocol {
    Oidc,
    Saml,
}

impl SsoProtocol {
    fn as_str(self) -> &'static str {
        match self {
            SsoProtocol::Oidc => "oidc",
            SsoProtocol::Saml => "saml",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "oidc" => Ok(SsoProtocol::Oidc),
            "saml" => Ok(SsoProtocol::Saml),
            _ => Err(anyhow::anyhow!("unsupported SSO protocol: {raw}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdpConnectionUpsert {
    pub account_id: String,
    pub connection_id: String,
    pub protocol: SsoProtocol,
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub client_secret_handle: Option<String>,
    pub allowed_domains: Vec<String>,
    pub scopes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdpConnection {
    pub account_id: String,
    pub id: String,
    pub protocol: SsoProtocol,
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    #[serde(skip_serializing)]
    pub client_secret_handle: Option<String>,
    pub allowed_domains: Vec<String>,
    pub scopes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedIdpConnection {
    pub account_id: String,
    pub id: String,
    pub protocol: SsoProtocol,
    pub enabled: bool,
    pub issuer: String,
    pub client_id: String,
    pub client_secret_configured: bool,
    pub allowed_domains: Vec<String>,
    pub scopes: String,
}

impl IdpConnection {
    pub fn redacted(&self) -> RedactedIdpConnection {
        RedactedIdpConnection {
            account_id: self.account_id.clone(),
            id: self.id.clone(),
            protocol: self.protocol,
            enabled: self.enabled,
            issuer: self.issuer.clone(),
            client_id: self.client_id.clone(),
            client_secret_configured: self.client_secret_handle.is_some(),
            allowed_domains: self.allowed_domains.clone(),
            scopes: self.scopes.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedactedIdpRoleMapping {
    pub id: String,
    pub account_id: String,
    pub connection_id: String,
    pub idp_claim_key: String,
    pub idp_claim_value: String,
    pub role_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcLoginStart {
    pub account_id: String,
    pub connection_id: String,
    pub authorization_url: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedOidcClaims {
    pub issuer: String,
    pub audience: String,
    pub subject: String,
    pub email: String,
    pub email_verified: bool,
    pub nonce: String,
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OidcPendingLogin {
    pub state: String,
    pub account_id: String,
    pub connection_id: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SsoSession {
    pub account_id: String,
    pub connection_id: String,
    pub subject: String,
    pub email: String,
    pub roles: Vec<String>,
}

// ---------------------------------------------------------------------------
// OIDC discovery helpers (deserialized from .well-known endpoints)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OidcDiscovery {
    token_endpoint: String,
    #[allow(dead_code)]
    jwks_uri: String,
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkKey>,
}

#[derive(Debug, Deserialize)]
struct JwkKey {
    kid: Option<String>,
    kty: String,
    #[serde(rename = "use")]
    use_: Option<String>,
    n: Option<String>,
    e: Option<String>,
    // Other fields ignored
}

// ---------------------------------------------------------------------------
// Schema SQL (constants — used by migrations, not executed directly)
// ---------------------------------------------------------------------------

/// SQLite-flavoured DDL for SSO support.
///
/// Note: SQLite does not support `ALTER TABLE … ADD COLUMN IF NOT EXISTS`.
/// Use [`apply_sso_schema`] instead which gracefully handles duplicate columns.
pub const SSO_SCHEMA_SQL: &str = r#"
-- Add SSO fields to tenants table
ALTER TABLE tenants ADD COLUMN sso_enabled INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tenants ADD COLUMN oidc_issuer TEXT;
ALTER TABLE tenants ADD COLUMN oidc_client_id TEXT;
ALTER TABLE tenants ADD COLUMN oidc_client_secret TEXT;
ALTER TABLE tenants ADD COLUMN oidc_scopes TEXT DEFAULT 'openid email profile';
ALTER TABLE tenants ADD COLUMN allowed_domains TEXT DEFAULT '[]';

-- SSO role mappings table
CREATE TABLE IF NOT EXISTS sso_role_mappings (
    id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    idp_claim_key TEXT NOT NULL DEFAULT 'groups',
    idp_claim_value TEXT NOT NULL,
    role_id TEXT NOT NULL REFERENCES roles(name) ON DELETE CASCADE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(tenant_id, idp_claim_key, idp_claim_value, role_id)
);

CREATE INDEX IF NOT EXISTS idx_sso_role_mappings_tenant ON sso_role_mappings(tenant_id);
"#;

/// Apply the SSO schema to a SQLite database, ignoring "duplicate column" errors.
pub fn apply_sso_schema(conn: &Connection) -> Result<()> {
    let alter_stmts = [
        "ALTER TABLE tenants ADD COLUMN sso_enabled INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE tenants ADD COLUMN oidc_issuer TEXT",
        "ALTER TABLE tenants ADD COLUMN oidc_client_id TEXT",
        "ALTER TABLE tenants ADD COLUMN oidc_client_secret TEXT",
        "ALTER TABLE tenants ADD COLUMN oidc_scopes TEXT DEFAULT 'openid email profile'",
        "ALTER TABLE tenants ADD COLUMN allowed_domains TEXT DEFAULT '[]'",
    ];
    for stmt in &alter_stmts {
        match conn.execute_batch(stmt) {
            Ok(_) => {}
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate column") {
                    // Column already exists — that's fine.
                } else {
                    return Err(e).with_context(|| format!("failed to run: {stmt}"));
                }
            }
        }
    }

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sso_role_mappings (
            id TEXT PRIMARY KEY,
            tenant_id TEXT NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
            idp_claim_key TEXT NOT NULL DEFAULT 'groups',
            idp_claim_value TEXT NOT NULL,
            role_id TEXT NOT NULL REFERENCES roles(name) ON DELETE CASCADE,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            UNIQUE(tenant_id, idp_claim_key, idp_claim_value, role_id)
        );
        CREATE INDEX IF NOT EXISTS idx_sso_role_mappings_tenant ON sso_role_mappings(tenant_id);

        CREATE TABLE IF NOT EXISTS sso_idp_connections (
            account_id TEXT NOT NULL,
            id TEXT NOT NULL,
            protocol TEXT NOT NULL CHECK (protocol IN ('oidc', 'saml')),
            enabled INTEGER NOT NULL DEFAULT 1,
            issuer TEXT NOT NULL,
            client_id TEXT NOT NULL,
            client_secret_handle TEXT,
            allowed_domains TEXT NOT NULL DEFAULT '[]',
            scopes TEXT NOT NULL DEFAULT 'openid email profile',
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            PRIMARY KEY (account_id, id)
        );
        CREATE INDEX IF NOT EXISTS idx_sso_idp_connections_account ON sso_idp_connections(account_id);

        CREATE TABLE IF NOT EXISTS sso_idp_role_mappings (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            idp_claim_key TEXT NOT NULL DEFAULT 'groups',
            idp_claim_value TEXT NOT NULL,
            role_id TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            UNIQUE(account_id, connection_id, idp_claim_key, idp_claim_value, role_id),
            FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS sso_oidc_login_transactions (
            state TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            nonce TEXT NOT NULL,
            pkce_verifier TEXT NOT NULL,
            redirect_uri TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
            consumed_at TEXT,
            FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS sso_sessions (
            id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            connection_id TEXT NOT NULL,
            subject TEXT NOT NULL,
            email TEXT NOT NULL,
            roles TEXT NOT NULL DEFAULT '[]',
            issued_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            revoked_at INTEGER,
            FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_sso_sessions_account ON sso_sessions(account_id);
        CREATE INDEX IF NOT EXISTS idx_sso_sessions_subject ON sso_sessions(account_id, subject);",
    )
    .context("failed to create sso_role_mappings table")?;

    Ok(())
}

// ---------------------------------------------------------------------------
// SsoManager
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct SsoManager {
    conn: Arc<Mutex<Connection>>,
}

impl SsoManager {
    pub fn new(db: Arc<Mutex<Connection>>) -> Self {
        Self { conn: db }
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let conn = Connection::open(path).context("failed to open SSO catalog")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tenants (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS roles (
                name TEXT PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'PUBLIC'
            );
            CREATE TABLE IF NOT EXISTS user_roles (
                username TEXT NOT NULL,
                role_name TEXT NOT NULL,
                PRIMARY KEY (username, role_name)
            );
            INSERT OR IGNORE INTO roles (name) VALUES ('PUBLIC');
            INSERT OR IGNORE INTO roles (name) VALUES ('SYSADMIN');",
        )?;
        apply_sso_schema(&conn)?;
        Ok(Self::new(Arc::new(Mutex::new(conn))))
    }

    /// Look up the tenant whose `allowed_domains` contains the email's domain.
    pub fn get_tenant_by_domain(&self, email: &str) -> Result<Option<TenantSsoConfig>> {
        let domain = email
            .rsplit_once('@')
            .map(|(_, d)| d.to_lowercase())
            .ok_or_else(|| anyhow::anyhow!("invalid email: missing @"))?;

        let db = self.conn.lock().unwrap();
        let mut stmt = db
            .prepare(
                "SELECT id, slug, name, sso_enabled, oidc_issuer, oidc_client_id,
                        oidc_client_secret, oidc_scopes, allowed_domains
                 FROM tenants
                 WHERE sso_enabled = 1",
            )
            .context("failed to query tenants")?;

        let rows = stmt.query_map([], |row| {
            Ok(TenantSsoConfig {
                id: row.get(0)?,
                slug: row.get(1)?,
                name: row.get(2)?,
                sso_enabled: row.get::<_, i32>(3)? != 0,
                oidc_issuer: row.get(4)?,
                oidc_client_id: row.get(5)?,
                oidc_client_secret: row.get(6)?,
                oidc_scopes: row
                    .get::<_, Option<String>>(7)?
                    .unwrap_or_else(|| "openid email profile".into()),
                allowed_domains: {
                    let raw: String = row
                        .get::<_, Option<String>>(8)?
                        .unwrap_or_else(|| "[]".into());
                    serde_json::from_str(&raw).unwrap_or_default()
                },
            })
        })?;

        for row in rows {
            let tenant = row.context("failed to read tenant row")?;
            if tenant
                .allowed_domains
                .iter()
                .any(|d| d.to_lowercase() == domain)
            {
                return Ok(Some(tenant));
            }
        }

        Ok(None)
    }

    /// Get all SSO role mappings for a tenant.
    pub fn get_sso_role_mappings(&self, tenant_id: &str) -> Result<Vec<SsoRoleMapping>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id, tenant_id, idp_claim_key, idp_claim_value, role_id
             FROM sso_role_mappings
             WHERE tenant_id = ?1",
        )?;

        let mappings = stmt
            .query_map(rusqlite::params![tenant_id], |row| {
                Ok(SsoRoleMapping {
                    id: row.get(0)?,
                    tenant_id: row.get(1)?,
                    idp_claim_key: row.get(2)?,
                    idp_claim_value: row.get(3)?,
                    role_id: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to read sso_role_mappings")?;

        Ok(mappings)
    }

    /// Given OIDC claims and a tenant's role mappings, return the role_ids that
    /// match.  Supports both string and array claim values from the IdP.
    pub fn apply_sso_role_mappings(
        &self,
        tenant_id: &str,
        claims: &OidcClaims,
    ) -> Result<Vec<String>> {
        let mappings = self.get_sso_role_mappings(tenant_id)?;
        let mut matched_role_ids: Vec<String> = Vec::new();

        for mapping in &mappings {
            if let Some(claim_val) = claims.extra.get(&mapping.idp_claim_key) {
                let matches = match claim_val {
                    serde_json::Value::String(s) => {
                        s.eq_ignore_ascii_case(&mapping.idp_claim_value)
                    }
                    serde_json::Value::Array(arr) => arr.iter().any(|v| {
                        v.as_str()
                            .map(|s| s.eq_ignore_ascii_case(&mapping.idp_claim_value))
                            .unwrap_or(false)
                    }),
                    _ => false,
                };
                if matches && !matched_role_ids.contains(&mapping.role_id) {
                    matched_role_ids.push(mapping.role_id.clone());
                }
            }
        }

        Ok(matched_role_ids)
    }

    pub fn upsert_idp_connection(&self, req: IdpConnectionUpsert) -> Result<IdpConnection> {
        validate_safe_id("account_id", &req.account_id)?;
        validate_safe_id("connection_id", &req.connection_id)?;
        if req.issuer.trim().is_empty() || req.client_id.trim().is_empty() {
            return Err(anyhow::anyhow!("issuer and client_id are required"));
        }
        if req.allowed_domains.is_empty() {
            return Err(anyhow::anyhow!("at least one allowed domain is required"));
        }
        if req
            .client_secret
            .as_ref()
            .is_some_and(|s| !s.trim().is_empty())
            && req
                .client_secret_handle
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            return Err(anyhow::anyhow!(
                "provide either oidc client_secret for local sealing or client_secret_handle, not both"
            ));
        }
        let provided_handle = req
            .client_secret_handle
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let local_handle = req
            .client_secret
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .map(|_| Self::idp_client_secret_handle(&req.account_id, &req.connection_id));
        let secret_handle = provided_handle.or(local_handle);
        if let (Some(raw_secret), Some(handle_id)) =
            (req.client_secret.as_deref(), secret_handle.as_deref())
        {
            self.store_idp_client_secret(&req.account_id, &req.issuer, handle_id, raw_secret)?;
        }
        let domains_json = serde_json::to_string(
            &req.allowed_domains
                .iter()
                .map(|d| d.trim().trim_start_matches('@').to_ascii_lowercase())
                .filter(|d| !d.is_empty())
                .collect::<Vec<_>>(),
        )?;
        let scopes = req
            .scopes
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "openid email profile".to_string());
        {
            let db = self.conn.lock().unwrap();
            db.execute(
                "INSERT INTO sso_idp_connections
                 (account_id, id, protocol, enabled, issuer, client_id, client_secret_handle, allowed_domains, scopes, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
                 ON CONFLICT(account_id, id) DO UPDATE SET
                    protocol = excluded.protocol,
                    enabled = excluded.enabled,
                    issuer = excluded.issuer,
                    client_id = excluded.client_id,
                    client_secret_handle = COALESCE(excluded.client_secret_handle, sso_idp_connections.client_secret_handle),
                    allowed_domains = excluded.allowed_domains,
                    scopes = excluded.scopes,
                    updated_at = excluded.updated_at",
                rusqlite::params![
                    req.account_id,
                    req.connection_id,
                    req.protocol.as_str(),
                    if req.enabled { 1 } else { 0 },
                    req.issuer,
                    req.client_id,
                    secret_handle,
                    domains_json,
                    scopes,
                ],
            )?;
        }
        self.get_idp_connection(&req.account_id, &req.connection_id)?
            .ok_or_else(|| anyhow::anyhow!("idp connection was not persisted"))
    }

    fn idp_client_secret_handle(account_id: &str, connection_id: &str) -> String {
        format!("secret://sso/{account_id}/{connection_id}/client_secret")
    }

    fn local_secret_store(&self) -> Result<TrustedSecretStore> {
        let master_key = std::env::var("OPENSNOW_SSO_SECRET_STORE_KEY")
            .unwrap_or_else(|_| "opensnow-local-sso-dev-key".to_string());
        TrustedSecretStore::local_dev(self.conn.clone(), &master_key)
    }

    fn store_idp_client_secret(
        &self,
        account_id: &str,
        issuer: &str,
        handle_id: &str,
        raw_secret: &str,
    ) -> Result<()> {
        let store = self.local_secret_store()?;
        let descriptor = SecretHandleDescriptor::new(
            account_id,
            handle_id,
            SecretType::IdpClientSecret,
            SecretPurpose::IdentityProviderClient,
        )
        .with_resource_scope(format!("oidc://{}", issuer.trim_end_matches('/')));
        match store.create_secret(descriptor, raw_secret) {
            Ok(_) => Ok(()),
            Err(_) => store
                .rotate_secret(account_id, handle_id, raw_secret)
                .map(|_| ()),
        }
    }

    fn resolve_idp_client_secret(&self, account_id: &str, handle_id: &str) -> Result<String> {
        if handle_id.starts_with("aws-secretsmanager://")
            || handle_id.starts_with("gcp-secretmanager://")
            || handle_id.starts_with("vault://")
        {
            return Ok(ExternalSecretResolver::from_handle(handle_id)?
                .resolve()?
                .expose_to_trusted_execution_path()
                .to_string());
        }
        Ok(self
            .local_secret_store()?
            .resolve_secret(account_id, handle_id)?
            .expose_to_trusted_execution_path()
            .to_string())
    }

    pub fn list_idp_connections(&self, account_id: &str) -> Result<Vec<RedactedIdpConnection>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT account_id, id, protocol, enabled, issuer, client_id, client_secret_handle, allowed_domains, scopes
             FROM sso_idp_connections WHERE account_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(rusqlite::params![account_id], idp_from_row)?;
        rows.map(|r| r.map(|c| c.redacted()).map_err(Into::into))
            .collect()
    }

    pub fn find_oidc_connection_by_email(&self, email: &str) -> Result<Option<IdpConnection>> {
        let domain = email
            .rsplit_once('@')
            .map(|(_, d)| d.to_ascii_lowercase())
            .ok_or_else(|| anyhow::anyhow!("invalid email: missing @"))?;
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT account_id, id, protocol, enabled, issuer, client_id, client_secret_handle, allowed_domains, scopes
             FROM sso_idp_connections WHERE enabled = 1 ORDER BY account_id, id",
        )?;
        let rows = stmt.query_map([], idp_from_row)?;
        for row in rows {
            let conn = row?;
            if conn
                .allowed_domains
                .iter()
                .any(|d| d.eq_ignore_ascii_case(&domain))
            {
                return Ok(Some(conn));
            }
        }
        Ok(None)
    }

    pub fn get_idp_connection(
        &self,
        account_id: &str,
        connection_id: &str,
    ) -> Result<Option<IdpConnection>> {
        let db = self.conn.lock().unwrap();
        match db.query_row(
            "SELECT account_id, id, protocol, enabled, issuer, client_id, client_secret_handle, allowed_domains, scopes
             FROM sso_idp_connections WHERE account_id = ?1 AND id = ?2",
            rusqlite::params![account_id, connection_id],
            idp_from_row,
        ) {
            Ok(conn) => Ok(Some(conn)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn delete_idp_connection(&self, account_id: &str, connection_id: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "DELETE FROM sso_idp_connections WHERE account_id = ?1 AND id = ?2",
            rusqlite::params![account_id, connection_id],
        )?;
        Ok(())
    }

    pub fn upsert_idp_role_mapping(
        &self,
        account_id: &str,
        connection_id: &str,
        idp_claim_key: &str,
        idp_claim_value: &str,
        role_id: &str,
    ) -> Result<RedactedIdpRoleMapping> {
        validate_safe_id("account_id", account_id)?;
        validate_safe_id("connection_id", connection_id)?;
        validate_safe_id("role_id", role_id)?;
        let id =
            format!("{account_id}:{connection_id}:{idp_claim_key}:{idp_claim_value}:{role_id}");
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO sso_idp_role_mappings (id, account_id, connection_id, idp_claim_key, idp_claim_value, role_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(account_id, connection_id, idp_claim_key, idp_claim_value, role_id) DO UPDATE SET role_id = excluded.role_id",
            rusqlite::params![id, account_id, connection_id, idp_claim_key, idp_claim_value, role_id],
        )?;
        Ok(RedactedIdpRoleMapping {
            id,
            account_id: account_id.to_string(),
            connection_id: connection_id.to_string(),
            idp_claim_key: idp_claim_key.to_string(),
            idp_claim_value: idp_claim_value.to_string(),
            role_id: role_id.to_string(),
        })
    }

    pub fn list_idp_role_mappings(&self, account_id: &str) -> Result<Vec<RedactedIdpRoleMapping>> {
        validate_safe_id("account_id", account_id)?;
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT id, account_id, connection_id, idp_claim_key, idp_claim_value, role_id
             FROM sso_idp_role_mappings WHERE account_id = ?1 ORDER BY connection_id, idp_claim_key, idp_claim_value, role_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![account_id], |row| {
            Ok(RedactedIdpRoleMapping {
                id: row.get(0)?,
                account_id: row.get(1)?,
                connection_id: row.get(2)?,
                idp_claim_key: row.get(3)?,
                idp_claim_value: row.get(4)?,
                role_id: row.get(5)?,
            })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn delete_idp_role_mapping(&self, account_id: &str, mapping_id: &str) -> Result<bool> {
        validate_safe_id("account_id", account_id)?;
        let db = self.conn.lock().unwrap();
        let changed = db.execute(
            "DELETE FROM sso_idp_role_mappings WHERE account_id = ?1 AND id = ?2",
            rusqlite::params![account_id, mapping_id],
        )?;
        Ok(changed > 0)
    }

    pub fn start_oidc_login(
        &self,
        account_id: &str,
        connection_id: &str,
        redirect_uri: &str,
    ) -> Result<OidcLoginStart> {
        let conn = self
            .get_idp_connection(account_id, connection_id)?
            .ok_or_else(|| anyhow::anyhow!("idp connection not found"))?;
        if !conn.enabled {
            return Err(anyhow::anyhow!("idp connection is disabled"));
        }
        if conn.protocol != SsoProtocol::Oidc {
            return Err(anyhow::anyhow!(
                "SAML SSO is not supported by OpenSnow's embedded enterprise auth yet; configure an external broker or use OIDC"
            ));
        }
        let state = random_token(48);
        let nonce = random_token(48);
        let pkce_verifier = random_token(64);
        let code_challenge = pkce_s256_challenge(&pkce_verifier);
        {
            let db = self.conn.lock().unwrap();
            db.execute(
                "INSERT INTO sso_oidc_login_transactions (state, account_id, connection_id, nonce, pkce_verifier, redirect_uri)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![state, account_id, connection_id, nonce, pkce_verifier, redirect_uri],
            )?;
        }
        let authorization_url = format!(
            "{}/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
            conn.issuer.trim_end_matches('/'),
            percent_encode(&conn.client_id),
            percent_encode(redirect_uri),
            percent_encode(&conn.scopes),
            percent_encode(&state),
            percent_encode(&nonce),
            percent_encode(&code_challenge),
        );
        Ok(OidcLoginStart {
            account_id: account_id.to_string(),
            connection_id: connection_id.to_string(),
            authorization_url,
            state,
            nonce,
            pkce_verifier,
        })
    }

    pub fn pending_oidc_login(&self, state: &str) -> Result<OidcPendingLogin> {
        let db = self.conn.lock().unwrap();
        db.query_row(
            "SELECT state, account_id, connection_id, nonce, pkce_verifier, redirect_uri
             FROM sso_oidc_login_transactions
             WHERE state = ?1 AND consumed_at IS NULL",
            rusqlite::params![state],
            |row| {
                Ok(OidcPendingLogin {
                    state: row.get(0)?,
                    account_id: row.get(1)?,
                    connection_id: row.get(2)?,
                    nonce: row.get(3)?,
                    pkce_verifier: row.get(4)?,
                    redirect_uri: row.get(5)?,
                })
            },
        )
        .context("invalid or consumed OIDC state")
    }

    pub fn verified_claims_from_oidc_claims(
        &self,
        claims: OidcClaims,
    ) -> Result<VerifiedOidcClaims> {
        let issuer = claims
            .iss
            .or_else(|| {
                claims
                    .extra
                    .get("iss")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("OIDC token missing issuer"))?;
        let audience = match claims.aud.or_else(|| claims.extra.get("aud").cloned()) {
            Some(serde_json::Value::String(s)) => s,
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .find_map(|v| v.as_str().map(str::to_string))
                .ok_or_else(|| anyhow::anyhow!("OIDC token missing audience"))?,
            _ => return Err(anyhow::anyhow!("OIDC token missing audience")),
        };
        let nonce = claims
            .nonce
            .or_else(|| {
                claims
                    .extra
                    .get("nonce")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .ok_or_else(|| anyhow::anyhow!("OIDC token missing nonce"))?;
        let email = claims
            .email
            .ok_or_else(|| anyhow::anyhow!("OIDC token missing email"))?;
        let email_verified = claims
            .email_verified
            .or_else(|| claims.extra.get("email_verified").and_then(|v| v.as_bool()))
            .unwrap_or(false);
        Ok(VerifiedOidcClaims {
            issuer,
            audience,
            subject: claims.sub,
            email,
            email_verified,
            nonce,
            extra: claims.extra,
        })
    }

    pub fn complete_oidc_login(
        &self,
        state: &str,
        pkce_verifier: &str,
        claims: VerifiedOidcClaims,
    ) -> Result<SsoSession> {
        let (account_id, connection_id, expected_nonce, expected_pkce): (
            String,
            String,
            String,
            String,
        ) = {
            let db = self.conn.lock().unwrap();
            db.query_row(
                "SELECT account_id, connection_id, nonce, pkce_verifier
                 FROM sso_oidc_login_transactions
                 WHERE state = ?1 AND consumed_at IS NULL",
                rusqlite::params![state],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .context("invalid or consumed OIDC state")?
        };
        if expected_pkce != pkce_verifier {
            return Err(anyhow::anyhow!("invalid OIDC PKCE verifier"));
        }
        if expected_nonce != claims.nonce {
            return Err(anyhow::anyhow!("invalid OIDC nonce"));
        }
        if !claims.email_verified {
            return Err(anyhow::anyhow!("OIDC email_verified claim is required"));
        }
        let conn = self
            .get_idp_connection(&account_id, &connection_id)?
            .ok_or_else(|| anyhow::anyhow!("idp connection not found"))?;
        if claims.issuer != conn.issuer {
            return Err(anyhow::anyhow!("OIDC issuer mismatch"));
        }
        if claims.audience != conn.client_id {
            return Err(anyhow::anyhow!("OIDC audience mismatch"));
        }
        let email_domain = claims
            .email
            .rsplit_once('@')
            .map(|(_, d)| d.to_ascii_lowercase())
            .ok_or_else(|| anyhow::anyhow!("invalid OIDC email"))?;
        if !conn
            .allowed_domains
            .iter()
            .any(|d| d.eq_ignore_ascii_case(&email_domain))
        {
            return Err(anyhow::anyhow!("OIDC email domain is not allowed"));
        }
        let roles = self.apply_idp_role_mappings(&account_id, &connection_id, &claims.extra)?;
        let roles = if roles.is_empty() {
            vec!["PUBLIC".to_string()]
        } else {
            roles
        };
        {
            let db = self.conn.lock().unwrap();
            db.execute(
                "UPDATE sso_oidc_login_transactions SET consumed_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE state = ?1",
                rusqlite::params![state],
            )?;
        }
        Ok(SsoSession {
            account_id,
            connection_id,
            subject: claims.subject,
            email: claims.email,
            roles,
        })
    }

    fn apply_idp_role_mappings(
        &self,
        account_id: &str,
        connection_id: &str,
        claims: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<String>> {
        let db = self.conn.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT idp_claim_key, idp_claim_value, role_id
             FROM sso_idp_role_mappings
             WHERE account_id = ?1 AND connection_id = ?2 ORDER BY role_id",
        )?;
        let mappings = stmt.query_map(rusqlite::params![account_id, connection_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut roles = Vec::new();
        for row in mappings {
            let (key, expected, role) = row?;
            let Some(value) = claims.get(&key) else {
                continue;
            };
            let matched = match value {
                serde_json::Value::String(s) => s.eq_ignore_ascii_case(&expected),
                serde_json::Value::Array(values) => values.iter().any(|v| {
                    v.as_str()
                        .map(|s| s.eq_ignore_ascii_case(&expected))
                        .unwrap_or(false)
                }),
                _ => false,
            };
            if matched && !roles.contains(&role) {
                roles.push(role);
            }
        }
        Ok(roles)
    }

    /// Verify an OIDC id_token against the tenant's IdP JWKS endpoint.
    ///
    /// Fetches `{oidc_issuer}/.well-known/jwks.json`, finds the matching key,
    /// and verifies the JWT signature using the `jsonwebtoken` crate.
    pub async fn verify_oidc_token(
        &self,
        tenant: &TenantSsoConfig,
        id_token: &str,
    ) -> Result<OidcClaims> {
        let issuer = tenant
            .oidc_issuer
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("tenant has no oidc_issuer configured"))?;

        // Decode the token header to find the `kid`
        let header =
            jsonwebtoken::decode_header(id_token).context("failed to decode id_token header")?;

        // Fetch JWKS
        let jwks_url = format!("{}/.well-known/jwks.json", issuer.trim_end_matches('/'));
        let client = reqwest::Client::new();
        let jwks: JwksResponse = client
            .get(&jwks_url)
            .send()
            .await
            .context("failed to fetch JWKS")?
            .json()
            .await
            .context("failed to parse JWKS response")?;

        // Find the matching key
        let key = if let Some(kid) = &header.kid {
            jwks.keys
                .iter()
                .find(|k| k.kid.as_deref() == Some(kid))
                .ok_or_else(|| anyhow::anyhow!("no matching JWK found for kid: {kid}"))?
        } else {
            // If no kid in header, use the first RSA signing key
            jwks.keys
                .iter()
                .find(|k| k.kty == "RSA" && k.use_.as_deref() != Some("enc"))
                .ok_or_else(|| anyhow::anyhow!("no suitable RSA key found in JWKS"))?
        };

        // Build decoding key from RSA components
        let n = key
            .n
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("JWK missing 'n' component"))?;
        let e = key
            .e
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("JWK missing 'e' component"))?;
        let decoding_key = jsonwebtoken::DecodingKey::from_rsa_components(n, e)
            .context("failed to build RSA decoding key")?;

        // Validate
        let alg = header.alg;
        let mut validation = jsonwebtoken::Validation::new(alg);
        validation.set_issuer(&[issuer]);
        if let Some(ref client_id) = tenant.oidc_client_id {
            validation.set_audience(&[client_id]);
        }

        let token_data = jsonwebtoken::decode::<OidcClaims>(id_token, &decoding_key, &validation)
            .context("OIDC token verification failed")?;

        info!(sub = %token_data.claims.sub, "verified OIDC token");
        Ok(token_data.claims)
    }

    /// Exchange an OIDC authorization code for an id_token.
    ///
    /// Discovers the token endpoint from `{oidc_issuer}/.well-known/openid-configuration`,
    /// then POSTs the code exchange request.
    pub async fn exchange_oidc_code(
        &self,
        conn: &IdpConnection,
        code: &str,
        redirect_uri: &str,
        pkce_verifier: &str,
    ) -> Result<String> {
        let issuer = conn.issuer.as_str();
        let client_id = conn.client_id.as_str();
        let handle_id = conn
            .client_secret_handle
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("idp client secret is not configured"))?;
        let client_secret = self.resolve_idp_client_secret(&conn.account_id, handle_id)?;

        // Discover token endpoint
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            issuer.trim_end_matches('/')
        );
        let client = reqwest::Client::new();
        let discovery: OidcDiscovery = client
            .get(&discovery_url)
            .send()
            .await
            .context("failed to fetch OIDC discovery document")?
            .json()
            .await
            .context("failed to parse OIDC discovery document")?;

        // Exchange code for tokens
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("client_secret", client_secret.as_str()),
            ("code_verifier", pkce_verifier),
        ];

        let resp: HashMap<String, serde_json::Value> = client
            .post(&discovery.token_endpoint)
            .form(&params)
            .send()
            .await
            .context("token exchange request failed")?
            .json()
            .await
            .context("failed to parse token exchange response")?;

        let id_token = resp
            .get("id_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("token response missing id_token"))?
            .to_string();

        info!("exchanged OIDC authorization code for id_token");
        Ok(id_token)
    }

    pub async fn complete_oidc_code_login(&self, state: &str, code: &str) -> Result<SsoSession> {
        let pending = self.pending_oidc_login(state)?;
        let conn = self
            .get_idp_connection(&pending.account_id, &pending.connection_id)?
            .ok_or_else(|| anyhow::anyhow!("idp connection not found"))?;
        if conn.protocol != SsoProtocol::Oidc || !conn.enabled {
            return Err(anyhow::anyhow!(
                "OIDC connection is disabled or unsupported"
            ));
        }
        let id_token = self
            .exchange_oidc_code(&conn, code, &pending.redirect_uri, &pending.pkce_verifier)
            .await?;
        let tenant = TenantSsoConfig {
            id: conn.account_id.clone(),
            slug: conn.id.clone(),
            name: conn.account_id.clone(),
            sso_enabled: true,
            oidc_issuer: Some(conn.issuer.clone()),
            oidc_client_id: Some(conn.client_id.clone()),
            oidc_client_secret: None,
            oidc_scopes: conn.scopes.clone(),
            allowed_domains: conn.allowed_domains.clone(),
        };
        let raw_claims = self.verify_oidc_token(&tenant, &id_token).await?;
        let claims = self.verified_claims_from_oidc_claims(raw_claims)?;
        self.complete_oidc_login(state, &pending.pkce_verifier, claims)
    }

    pub fn create_sso_session(
        &self,
        account_id: &str,
        connection_id: &str,
        subject: &str,
        email: &str,
        roles: &[String],
        ttl_seconds: i64,
    ) -> Result<String> {
        validate_safe_id("account_id", account_id)?;
        validate_safe_id("connection_id", connection_id)?;
        if ttl_seconds <= 0 {
            return Err(anyhow::anyhow!("SSO session ttl must be positive"));
        }
        let session_id = format!("sso_{}", random_token(32));
        let issued_at = Utc::now().timestamp();
        let expires_at = issued_at + ttl_seconds;
        let roles_json = serde_json::to_string(roles)?;
        let db = self.conn.lock().unwrap();
        db.execute(
            "INSERT INTO sso_sessions (id, account_id, connection_id, subject, email, roles, issued_at, expires_at, revoked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            rusqlite::params![
                session_id,
                account_id,
                connection_id,
                subject,
                email,
                roles_json,
                issued_at,
                expires_at,
            ],
        )?;
        Ok(session_id)
    }

    pub fn validate_sso_session(
        &self,
        session_id: &str,
        account_id: &str,
        email: &str,
    ) -> Result<()> {
        let db = self.conn.lock().unwrap();
        let now = Utc::now().timestamp();
        let (stored_account, stored_email, expires_at, revoked_at): (
            String,
            String,
            i64,
            Option<i64>,
        ) = db
            .query_row(
                "SELECT account_id, email, expires_at, revoked_at FROM sso_sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .context("SSO session not found")?;
        if stored_account != account_id || stored_email != email {
            return Err(anyhow::anyhow!("SSO session email/account mismatch"));
        }
        if revoked_at.is_some() {
            return Err(anyhow::anyhow!("SSO session revoked"));
        }
        if expires_at <= now {
            return Err(anyhow::anyhow!("SSO session expired"));
        }
        Ok(())
    }

    pub fn revoke_sso_session(&self, session_id: &str) -> Result<()> {
        let db = self.conn.lock().unwrap();
        db.execute(
            "UPDATE sso_sessions SET revoked_at = ?2 WHERE id = ?1",
            rusqlite::params![session_id, Utc::now().timestamp()],
        )?;
        Ok(())
    }

    /// Upsert a user from SSO login.  Sets `auth_provider='oidc'` and syncs
    /// the supplied role names into the `user_roles` table.
    ///
    /// Returns the user's row id (as a String for consistency with UUIDs).
    pub fn upsert_sso_user(
        &self,
        tenant_id: &str,
        email: &str,
        name: Option<&str>,
        role_ids: &[String],
    ) -> Result<String> {
        let db = self.conn.lock().unwrap();

        // Derive a username from the email (local part)
        let username = email.split('@').next().unwrap_or(email);
        let _display = name.unwrap_or(username);

        // Check if user exists
        let existing: Option<i64> = db
            .query_row(
                "SELECT id FROM users WHERE username = ?1",
                rusqlite::params![email],
                |row| row.get(0),
            )
            .ok();

        let user_id: i64 = if let Some(id) = existing {
            // Update name/role if needed
            let primary_role = role_ids.first().map(|s| s.as_str()).unwrap_or("PUBLIC");
            db.execute(
                "UPDATE users SET role = ?1 WHERE id = ?2",
                rusqlite::params![primary_role, id],
            )?;
            id
        } else {
            // Insert new SSO user (no password — use a placeholder hash)
            let primary_role = role_ids.first().map(|s| s.as_str()).unwrap_or("PUBLIC");
            db.execute(
                "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
                rusqlite::params![email, "oidc:no-password", primary_role],
            )?;
            db.last_insert_rowid()
        };

        // Sync user_roles: clear existing, re-insert
        db.execute(
            "DELETE FROM user_roles WHERE username = ?1",
            rusqlite::params![email],
        )
        .ok(); // table may not exist if RoleStore wasn't initialised

        for role_id in role_ids {
            db.execute(
                "INSERT OR IGNORE INTO user_roles (username, role_name) VALUES (?1, ?2)",
                rusqlite::params![email, role_id],
            )
            .ok();
        }

        info!(
            user_id,
            email,
            tenant_id,
            roles = ?role_ids,
            "upserted SSO user"
        );

        Ok(user_id.to_string())
    }
}

fn idp_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IdpConnection> {
    let protocol: String = row.get(2)?;
    let domains: String = row.get(7)?;
    Ok(IdpConnection {
        account_id: row.get(0)?,
        id: row.get(1)?,
        protocol: SsoProtocol::parse(&protocol).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                )),
            )
        })?,
        enabled: row.get::<_, i32>(3)? != 0,
        issuer: row.get(4)?,
        client_id: row.get(5)?,
        client_secret_handle: row.get(6)?,
        allowed_domains: serde_json::from_str(&domains).unwrap_or_default(),
        scopes: row.get(8)?,
    })
}

fn validate_safe_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(anyhow::anyhow!("{field} must contain only [A-Za-z0-9_.-]"));
    }
    Ok(())
}

fn random_token(len: usize) -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn pkce_s256_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        // Create minimal tenants table
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tenants (
                id TEXT PRIMARY KEY,
                slug TEXT NOT NULL,
                name TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS roles (
                name TEXT PRIMARY KEY
            );
            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                role TEXT NOT NULL DEFAULT 'PUBLIC'
            );
            CREATE TABLE IF NOT EXISTS user_roles (
                username TEXT NOT NULL,
                role_name TEXT NOT NULL,
                PRIMARY KEY (username, role_name)
            );",
        )
        .unwrap();

        // Apply SSO schema
        apply_sso_schema(&conn).unwrap();

        // Insert a test tenant
        conn.execute(
            "INSERT INTO tenants (id, slug, name, sso_enabled, oidc_issuer, oidc_client_id, allowed_domains)
             VALUES ('t1', 'acme', 'Acme Corp', 1, 'https://idp.acme.com', 'client123', '[\"acme.com\", \"acme.io\"]')",
            [],
        ).unwrap();

        // Insert some roles
        conn.execute_batch(
            "INSERT INTO roles (name) VALUES ('SYSADMIN');
             INSERT INTO roles (name) VALUES ('PUBLIC');
             INSERT INTO roles (name) VALUES ('ANALYST');",
        )
        .unwrap();

        // Insert role mappings
        conn.execute(
            "INSERT INTO sso_role_mappings (id, tenant_id, idp_claim_key, idp_claim_value, role_id)
             VALUES ('m1', 't1', 'groups', 'engineering', 'SYSADMIN')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sso_role_mappings (id, tenant_id, idp_claim_key, idp_claim_value, role_id)
             VALUES ('m2', 't1', 'department', 'analytics', 'ANALYST')",
            [],
        )
        .unwrap();

        Arc::new(Mutex::new(conn))
    }

    #[test]
    fn test_get_tenant_by_domain() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);

        let tenant = mgr.get_tenant_by_domain("alice@acme.com").unwrap();
        assert!(tenant.is_some());
        let t = tenant.unwrap();
        assert_eq!(t.slug, "acme");
        assert!(t.sso_enabled);

        let tenant = mgr.get_tenant_by_domain("bob@acme.io").unwrap();
        assert!(tenant.is_some());

        let tenant = mgr.get_tenant_by_domain("eve@other.com").unwrap();
        assert!(tenant.is_none());
    }

    #[test]
    fn test_get_sso_role_mappings() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);

        let mappings = mgr.get_sso_role_mappings("t1").unwrap();
        assert_eq!(mappings.len(), 2);
    }

    #[test]
    fn test_apply_sso_role_mappings_array_claim() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);

        let claims = OidcClaims {
            sub: "user1".into(),
            iss: None,
            aud: None,
            nonce: None,
            email: Some("alice@acme.com".into()),
            email_verified: None,
            name: Some("Alice".into()),
            picture: None,
            preferred_username: None,
            extra: HashMap::from([(
                "groups".into(),
                serde_json::json!(["engineering", "devops"]),
            )]),
        };

        let roles = mgr.apply_sso_role_mappings("t1", &claims).unwrap();
        assert_eq!(roles, vec!["SYSADMIN"]);
    }

    #[test]
    fn test_apply_sso_role_mappings_string_claim() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);

        let claims = OidcClaims {
            sub: "user2".into(),
            iss: None,
            aud: None,
            nonce: None,
            email: Some("bob@acme.com".into()),
            email_verified: None,
            name: None,
            picture: None,
            preferred_username: None,
            extra: HashMap::from([("department".into(), serde_json::json!("analytics"))]),
        };

        let roles = mgr.apply_sso_role_mappings("t1", &claims).unwrap();
        assert_eq!(roles, vec!["ANALYST"]);
    }

    #[test]
    fn test_upsert_sso_user_new() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db.clone());

        let uid = mgr
            .upsert_sso_user("t1", "alice@acme.com", Some("Alice"), &["SYSADMIN".into()])
            .unwrap();
        assert!(!uid.is_empty());

        // Verify the user was created
        let conn = db.lock().unwrap();
        let role: String = conn
            .query_row(
                "SELECT role FROM users WHERE username = 'alice@acme.com'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(role, "SYSADMIN");
    }

    #[test]
    fn test_upsert_sso_user_existing() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);

        // First insert
        mgr.upsert_sso_user("t1", "carol@acme.com", Some("Carol"), &["PUBLIC".into()])
            .unwrap();

        // Second upsert should update role
        let uid = mgr
            .upsert_sso_user("t1", "carol@acme.com", Some("Carol"), &["SYSADMIN".into()])
            .unwrap();
        assert!(!uid.is_empty());
    }

    #[test]
    fn test_invalid_email() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);
        let result = mgr.get_tenant_by_domain("not-an-email");
        assert!(result.is_err());
    }

    #[test]
    fn idp_connection_crud_redacts_client_secret_and_persists_secret_handle() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db.clone());

        let saved = mgr
            .upsert_idp_connection(IdpConnectionUpsert {
                account_id: "acme".into(),
                connection_id: "okta".into(),
                protocol: SsoProtocol::Oidc,
                enabled: true,
                issuer: "https://idp.acme.test".into(),
                client_id: "opensnow".into(),
                client_secret: Some("super-secret-value".into()),
                client_secret_handle: None,
                allowed_domains: vec!["acme.test".into()],
                scopes: Some("openid email profile groups".into()),
            })
            .unwrap();

        assert_eq!(
            saved.client_secret_handle.as_deref(),
            Some("secret://sso/acme/okta/client_secret")
        );
        let api_json = serde_json::to_string(&saved.redacted()).unwrap();
        assert!(api_json.contains("client_secret_configured"));
        assert!(!api_json.contains("super-secret-value"));

        let persisted_secret: String = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT client_secret_handle FROM sso_idp_connections WHERE account_id = 'acme' AND id = 'okta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_secret, "secret://sso/acme/okta/client_secret");
        let raw_secret_count: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM sso_idp_connections WHERE client_secret_handle LIKE '%super-secret-value%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw_secret_count, 0);

        let listed = mgr.list_idp_connections("acme").unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].client_secret_configured);
        mgr.delete_idp_connection("acme", "okta").unwrap();
        assert!(mgr.get_idp_connection("acme", "okta").unwrap().is_none());
    }

    #[test]
    fn oidc_start_requires_csrf_state_nonce_and_pkce_and_saml_fails_closed() {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);
        mgr.upsert_idp_connection(IdpConnectionUpsert {
            account_id: "acme".into(),
            connection_id: "okta".into(),
            protocol: SsoProtocol::Oidc,
            enabled: true,
            issuer: "https://idp.acme.test".into(),
            client_id: "opensnow".into(),
            client_secret: Some("super-secret-value".into()),
            client_secret_handle: None,
            allowed_domains: vec!["acme.test".into()],
            scopes: None,
        })
        .unwrap();
        mgr.upsert_idp_connection(IdpConnectionUpsert {
            account_id: "acme".into(),
            connection_id: "saml".into(),
            protocol: SsoProtocol::Saml,
            enabled: true,
            issuer: "https://saml.acme.test".into(),
            client_id: "entity-id".into(),
            client_secret: None,
            client_secret_handle: None,
            allowed_domains: vec!["acme.test".into()],
            scopes: None,
        })
        .unwrap();

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        assert!(start.authorization_url.contains("response_type=code"));
        assert!(start.authorization_url.contains("code_challenge="));
        assert!(
            start
                .authorization_url
                .contains("code_challenge_method=S256")
        );
        assert!(
            !start
                .authorization_url
                .contains("code_challenge_method=plain")
        );
        assert!(
            !start
                .authorization_url
                .contains(&format!("code_challenge={}", start.pkce_verifier))
        );
        assert!(start.authorization_url.contains("nonce="));
        assert!(start.state.len() >= 32);
        assert!(start.nonce.len() >= 32);
        assert!(
            mgr.start_oidc_login("acme", "saml", "https://app.opensnow.test/callback")
                .is_err()
        );
    }

    #[test]
    fn oidc_completion_denies_invalid_issuer_audience_email_domain_state_nonce_pkce_and_maps_roles()
    {
        let db = setup_test_db();
        let mgr = SsoManager::new(db);
        mgr.upsert_idp_connection(IdpConnectionUpsert {
            account_id: "acme".into(),
            connection_id: "okta".into(),
            protocol: SsoProtocol::Oidc,
            enabled: true,
            issuer: "https://idp.acme.test".into(),
            client_id: "opensnow".into(),
            client_secret: Some("super-secret-value".into()),
            client_secret_handle: None,
            allowed_domains: vec!["acme.test".into()],
            scopes: None,
        })
        .unwrap();
        mgr.upsert_idp_role_mapping("acme", "okta", "groups", "engineering", "SYSADMIN")
            .unwrap();
        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let claims = VerifiedOidcClaims {
            issuer: "https://idp.acme.test".into(),
            audience: "opensnow".into(),
            subject: "user-123".into(),
            email: "alice@acme.test".into(),
            email_verified: true,
            nonce: start.nonce.clone(),
            extra: HashMap::from([("groups".into(), serde_json::json!(["engineering"]))]),
        };

        assert!(
            mgr.complete_oidc_login("missing-state", &start.pkce_verifier, claims.clone())
                .is_err()
        );
        assert!(
            mgr.complete_oidc_login(&start.state, "wrong-verifier", claims.clone())
                .is_err()
        );
        let mut bad = claims.clone();
        bad.nonce = "wrong".into();
        assert!(
            mgr.complete_oidc_login(&start.state, &start.pkce_verifier, bad)
                .is_err()
        );

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let mut bad = claims.clone();
        bad.issuer = "https://evil.test".into();
        bad.nonce = start.nonce.clone();
        assert!(
            mgr.complete_oidc_login(&start.state, &start.pkce_verifier, bad)
                .is_err()
        );

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let mut bad = claims.clone();
        bad.audience = "other-client".into();
        bad.nonce = start.nonce.clone();
        assert!(
            mgr.complete_oidc_login(&start.state, &start.pkce_verifier, bad)
                .is_err()
        );

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let mut bad = claims.clone();
        bad.email = "mallory@evil.test".into();
        bad.nonce = start.nonce.clone();
        assert!(
            mgr.complete_oidc_login(&start.state, &start.pkce_verifier, bad)
                .is_err()
        );

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let mut bad = claims.clone();
        bad.email_verified = false;
        bad.nonce = start.nonce.clone();
        assert!(
            mgr.complete_oidc_login(&start.state, &start.pkce_verifier, bad)
                .is_err()
        );

        let start = mgr
            .start_oidc_login("acme", "okta", "https://app.opensnow.test/callback")
            .unwrap();
        let mut good = claims;
        good.nonce = start.nonce.clone();
        let session = mgr
            .complete_oidc_login(&start.state, &start.pkce_verifier, good)
            .unwrap();
        assert_eq!(session.account_id, "acme");
        assert_eq!(session.email, "alice@acme.test");
        assert_eq!(session.roles, vec!["SYSADMIN"]);
    }
}
