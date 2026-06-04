//! JWT auth + OAuth2 `client_credentials` token endpoint.
//!
//! Two pieces:
//!
//! * [`jwt_required`] — axum middleware that rejects requests missing a
//!   valid `Authorization: Bearer *** header.
//! * [`auth_router`] — exposes `POST /auth/token` (the
//!   `client_credentials` grant) backed by an in-memory
//!   [`ClientRegistry`].
//!
//! The same [`AuthState`] is shared between both: the registry is consulted
//! by the token endpoint, and the [`JwtManager`] is consulted by the
//! middleware. Routes that should be protected are layered with
//! [`jwt_required`] via `route_layer` in [`crate::rest`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{FromRequestParts, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::Response,
    routing::{delete, get, post},
};
use jsonwebtoken;
use opensnow_auth::{
    AuditEvent, AuditResult, Claims, EnterpriseJwtConfig, EnterpriseJwtKey, JsonWebKey, JwtManager,
};
use opensnow_catalog::{
    Catalog, ScimTokenInput, ServiceIdentityClient, ServiceIdentityClientInput,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};

pub const DEFAULT_EVALUATION_QUERY_QUOTA: u64 = 100;

/// Whether a registered client is a customer-owned enterprise identity or an
/// OpenSnow-hosted evaluation/sandbox identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientAccountKind {
    Enterprise,
    Evaluation,
}

/// Operator-controlled lifecycle state for registered clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientStatus {
    Active,
    Suspended,
    Revoked,
}

/// Safe operator-facing account snapshot. Never includes password hashes or raw
/// client secrets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientSnapshot {
    pub client_id: String,
    pub role: String,
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub kind: ClientAccountKind,
    pub status: ClientStatus,
    pub status_reason: Option<String>,
    pub query_quota: Option<u64>,
    pub queries_used: u64,
}

/// A registered API client. Authenticates against the
/// [`token_endpoint`] using `client_id` + `client_secret` and
/// receives a JWT carrying the configured role.
#[derive(Debug, Clone)]
pub struct RegisteredClient {
    pub secret_hash: String,
    pub role: String,
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub kind: ClientAccountKind,
    pub status: ClientStatus,
    pub status_reason: Option<String>,
    pub query_quota: Option<u64>,
    pub queries_used: u64,
    pub account_id: Option<String>,
    pub workspace_id: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub rotated_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: Option<String>,
}

impl RegisteredClient {
    fn hash_secret(secret: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(secret.as_bytes(), &salt)
            .expect("argon2 hashing uses valid generated salt")
            .to_string()
    }

    fn new(secret: &str, role: &str) -> Self {
        let secret_hash = Self::hash_secret(secret);
        Self {
            secret_hash,
            role: role.to_string(),
            tenant_id: "default".to_string(),
            scopes: vec![role.to_string()],
            kind: if role.eq_ignore_ascii_case("EVALUATION") {
                ClientAccountKind::Evaluation
            } else {
                ClientAccountKind::Enterprise
            },
            status: ClientStatus::Active,
            status_reason: None,
            query_quota: None,
            queries_used: 0,
            account_id: None,
            workspace_id: None,
            expires_at: None,
            last_used_at: None,
            rotated_at: None,
            revoked_at: None,
            created_at: None,
        }
    }

    fn from_durable(record: &ServiceIdentityClient) -> Option<Self> {
        let status = match record.status.as_str() {
            "active" => ClientStatus::Active,
            "suspended" => ClientStatus::Suspended,
            "revoked" => ClientStatus::Revoked,
            _ => return None,
        };
        Some(Self {
            secret_hash: record.secret_hash.clone(),
            role: record.role.clone(),
            tenant_id: record
                .workspace_id
                .clone()
                .unwrap_or_else(|| record.account_id.clone()),
            scopes: if record.scopes.is_empty() {
                vec![record.role.clone()]
            } else {
                record.scopes.clone()
            },
            kind: ClientAccountKind::Enterprise,
            status,
            status_reason: record.status_reason.clone(),
            query_quota: None,
            queries_used: 0,
            account_id: Some(record.account_id.clone()),
            workspace_id: record.workspace_id.clone(),
            expires_at: record.expires_at.clone(),
            last_used_at: record.last_used_at.clone(),
            rotated_at: record.rotated_at.clone(),
            revoked_at: record.revoked_at.clone(),
            created_at: Some(record.created_at.clone()),
        })
    }

    fn is_expired(&self) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|expires| chrono::DateTime::parse_from_rfc3339(expires).ok())
            .is_some_and(|expires| expires <= chrono::Utc::now())
    }

    fn verify_secret(&self, secret: &str) -> bool {
        if self.status != ClientStatus::Active || self.is_expired() {
            return false;
        }
        PasswordHash::new(&self.secret_hash)
            .ok()
            .and_then(|parsed| {
                Argon2::default()
                    .verify_password(secret.as_bytes(), &parsed)
                    .ok()
            })
            .is_some()
    }

    fn snapshot(&self, client_id: &str) -> ClientSnapshot {
        ClientSnapshot {
            client_id: client_id.to_string(),
            role: self.role.clone(),
            tenant_id: self.tenant_id.clone(),
            scopes: self.scopes.clone(),
            kind: self.kind.clone(),
            status: self.status.clone(),
            status_reason: self.status_reason.clone(),
            query_quota: self.query_quota,
            queries_used: self.queries_used,
        }
    }
}

/// In-memory `client_id` → secret + role map. Cheap to clone (`Arc`-shared
/// internals). For production, swap in a SQLite-backed implementation
/// behind the same `Send + Sync` interface.
#[derive(Clone, Default)]
pub struct ClientRegistry {
    inner: Arc<Mutex<HashMap<String, RegisteredClient>>>,
}

impl ClientRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a client. Existing entries are overwritten — the registry
    /// is intentionally idempotent so callers can re-issue from config on
    /// every startup without checking-then-inserting.
    pub fn register(&self, client_id: &str, secret: &str, role: &str) {
        self.register_with_metadata(client_id, secret, role, "default", vec![role.to_string()]);
    }

    /// Register a client with enterprise contract identity metadata.
    pub fn register_with_metadata(
        &self,
        client_id: &str,
        secret: &str,
        role: &str,
        tenant_id: &str,
        scopes: Vec<String>,
    ) {
        let mut client = RegisteredClient::new(secret, role);
        client.tenant_id = tenant_id.to_string();
        client.scopes = if scopes.is_empty() {
            vec![role.to_string()]
        } else {
            scopes
        };
        self.inner
            .lock()
            .unwrap()
            .insert(client_id.to_string(), client);
    }

    /// Register an isolated evaluation/sandbox client. Evaluation accounts are
    /// tenant-scoped, guarded by demo SQL limits, and can be suspended/revoked
    /// by operators without trusting previously issued bearer tokens.
    pub fn register_evaluation_client(
        &self,
        client_id: &str,
        secret: &str,
        tenant_id: &str,
        query_quota: u64,
    ) {
        let mut client = RegisteredClient::new(secret, "EVALUATION");
        client.tenant_id = tenant_id.to_string();
        client.scopes = vec!["sql.query".to_string(), "table.select".to_string()];
        client.kind = ClientAccountKind::Evaluation;
        client.query_quota = Some(query_quota);
        self.inner
            .lock()
            .unwrap()
            .insert(client_id.to_string(), client);
    }

    pub fn register_durable_client(&self, record: ServiceIdentityClient) -> Result<(), String> {
        let client = RegisteredClient::from_durable(&record)
            .ok_or_else(|| format!("unsupported service identity status: {}", record.status))?;
        self.inner.lock().unwrap().insert(record.id, client);
        Ok(())
    }

    pub fn load_durable_clients_from_catalog(&self, catalog_path: &str) -> Result<usize, String> {
        let catalog = Catalog::open(catalog_path).map_err(|e| e.to_string())?;
        let records = catalog
            .list_service_identity_clients(None)
            .map_err(|e| e.to_string())?;
        let loaded = records.len();
        for record in records {
            self.register_durable_client(record)?;
        }
        Ok(loaded)
    }

    pub fn refresh_durable_client_from_catalog(
        &self,
        catalog_path: &str,
        client_id: &str,
    ) -> Result<(), String> {
        let catalog = Catalog::open(catalog_path).map_err(|e| e.to_string())?;
        match catalog
            .get_service_identity_client(client_id)
            .map_err(|e| e.to_string())?
        {
            Some(record) => self.register_durable_client(record),
            None => Ok(()),
        }
    }

    pub fn secret_hash_for_secret(secret: &str) -> String {
        RegisteredClient::hash_secret(secret)
    }

    pub fn suspend_client(&self, client_id: &str, reason: &str) -> Result<(), String> {
        self.set_status(client_id, ClientStatus::Suspended, Some(reason.to_string()))
    }

    pub fn revoke_client(&self, client_id: &str, reason: &str) -> Result<(), String> {
        self.set_status(client_id, ClientStatus::Revoked, Some(reason.to_string()))
    }

    fn set_status(
        &self,
        client_id: &str,
        status: ClientStatus,
        reason: Option<String>,
    ) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        let client = map
            .get_mut(client_id)
            .ok_or_else(|| format!("unknown client_id: {client_id}"))?;
        client.status = status;
        client.status_reason = reason;
        Ok(())
    }

    pub fn authorize_bearer_client(
        &self,
        client_id: &str,
        scopes: &[String],
    ) -> Result<(), StatusCode> {
        let mut map = self.inner.lock().unwrap();
        let Some(client) = map.get_mut(client_id) else {
            // Human/SSO JWTs are not necessarily registered API clients.
            return Ok(());
        };
        match client.status {
            ClientStatus::Active => {}
            ClientStatus::Suspended | ClientStatus::Revoked => return Err(StatusCode::FORBIDDEN),
        }
        if client.is_expired() {
            return Err(StatusCode::FORBIDDEN);
        }
        if client.kind == ClientAccountKind::Evaluation
            && scopes
                .iter()
                .any(|scope| scope == "sql.query" || scope == "*")
        {
            if let Some(quota) = client.query_quota {
                if client.queries_used >= quota {
                    return Err(StatusCode::TOO_MANY_REQUESTS);
                }
            }
            client.queries_used += 1;
        }
        Ok(())
    }

    pub fn evaluation_snapshots(&self) -> Vec<ClientSnapshot> {
        let mut snapshots: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, client)| client.kind == ClientAccountKind::Evaluation)
            .map(|(client_id, client)| client.snapshot(client_id))
            .collect();
        snapshots.sort_by(|a, b| a.client_id.cmp(&b.client_id));
        snapshots
    }

    /// Verify a client_id/secret pair and return the associated service identity.
    pub fn authenticate_client(&self, client_id: &str, secret: &str) -> Option<RegisteredClient> {
        let map = self.inner.lock().unwrap();
        let entry = map.get(client_id)?;
        entry.verify_secret(secret).then(|| entry.clone())
    }

    /// Verify a client_id/secret pair and return the associated role.
    pub fn authenticate(&self, client_id: &str, secret: &str) -> Option<String> {
        self.authenticate_client(client_id, secret)
            .map(|entry| entry.role)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }

    #[cfg(test)]
    pub fn describe_client_for_test(&self, client_id: &str) -> Option<RegisteredClient> {
        self.inner.lock().unwrap().get(client_id).cloned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimTokenSnapshot {
    pub id: String,
    pub account_id: String,
    pub label: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimTokenIssue {
    pub id: String,
    pub account_id: String,
    pub label: String,
    pub secret: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
struct ScimTokenRecord {
    snapshot: ScimTokenSnapshot,
    secret_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimUserRecord {
    pub id: String,
    pub account_id: String,
    pub user_name: String,
    pub display_name: Option<String>,
    pub active: bool,
    pub lifecycle: String,
    pub external_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimGroupRecord {
    pub id: String,
    pub account_id: String,
    pub display_name: String,
    pub role: String,
    pub members: Vec<String>,
    pub tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Default)]
pub struct ScimDirectory {
    inner: Arc<Mutex<ScimDirectoryInner>>,
    catalog_path: Option<String>,
}

#[derive(Default)]
struct ScimDirectoryInner {
    token_counter: u64,
    user_counter: u64,
    group_counter: u64,
    tokens: HashMap<String, ScimTokenRecord>,
    users: HashMap<(String, String), ScimUserRecord>,
    users_by_name: HashMap<(String, String), String>,
    groups: HashMap<(String, String), ScimGroupRecord>,
    audit: Vec<Value>,
}

impl ScimDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn durable(catalog_path: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ScimDirectoryInner::default())),
            catalog_path: Some(catalog_path.into()),
        }
    }

    fn catalog(&self) -> Option<Catalog> {
        self.catalog_path
            .as_deref()
            .and_then(|path| match Catalog::open(path) {
                Ok(catalog) => Some(catalog),
                Err(e) => {
                    warn!("failed to open SCIM durable catalog: {e}");
                    None
                }
            })
    }

    fn append_audit(
        &self,
        account_id: &str,
        action: &str,
        actor: Option<&str>,
        resource_type: &str,
        resource_id: &str,
        metadata: Value,
    ) {
        let Some(catalog) = self.catalog() else {
            return;
        };
        let event = AuditEvent {
            event_time: chrono::Utc::now(),
            organization_id: account_id.to_string(),
            tenant_id: Some(account_id.to_string()),
            actor_type: actor.map(|_| "user").unwrap_or("scim_client").to_string(),
            actor_id: actor.unwrap_or("scim").to_string(),
            actor_display: actor.map(ToOwned::to_owned),
            actor_auth_method: Some("scim".to_string()),
            action: action.to_string(),
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            resource_name: None,
            result: AuditResult::Succeeded,
            trace_id: None,
            secret_handle_refs: Vec::new(),
            metadata_redacted: metadata.as_object().cloned().unwrap_or_default(),
        };
        if let Err(e) = catalog.append_audit_event(account_id, None, &event) {
            warn!("failed to append SCIM audit event: {e}");
        }
    }

    pub fn rotate_token(
        &self,
        account_id: &str,
        label: &str,
        actor: &str,
        break_glass: bool,
    ) -> ScimTokenIssue {
        let salt = SaltString::generate(&mut OsRng)
            .to_string()
            .replace('$', "");
        let mut inner = self.inner.lock().unwrap();
        inner.token_counter += 1;
        let id = self
            .catalog()
            .and_then(|catalog| catalog.next_scim_token_id(account_id).ok())
            .unwrap_or_else(|| format!("sct_{}_{}", account_id, inner.token_counter));
        let secret = format!("scim_{}_{}_{}", account_id, inner.token_counter, salt);
        let secret_hash = Argon2::default()
            .hash_password(secret.as_bytes(), &SaltString::generate(&mut OsRng))
            .expect("argon2 hashing uses valid generated salt")
            .to_string();
        let now = chrono::Utc::now().to_rfc3339();
        let snapshot = ScimTokenSnapshot {
            id: id.clone(),
            account_id: account_id.to_string(),
            label: if label.trim().is_empty() {
                "scim".to_string()
            } else {
                label.to_string()
            },
            created_at: now.clone(),
            revoked_at: None,
        };
        inner.tokens.insert(
            id.clone(),
            ScimTokenRecord {
                snapshot: snapshot.clone(),
                secret_hash: secret_hash.clone(),
            },
        );
        inner.audit.push(json!({
            "action": "scim.token.rotate",
            "account_id": account_id,
            "actor": actor,
            "break_glass": break_glass,
            "token_id": id,
            "at": now,
        }));
        if let Some(catalog) = self.catalog() {
            if let Err(e) = catalog.upsert_scim_token(&ScimTokenInput {
                id: id.clone(),
                account_id: account_id.to_string(),
                label: snapshot.label.clone(),
                secret_hash: secret_hash.clone(),
                created_at: snapshot.created_at.clone(),
            }) {
                warn!("failed to persist SCIM token: {e}");
            }
            self.append_audit(
                account_id,
                "scim.token.rotate",
                Some(actor),
                "scim_token",
                &id,
                json!({ "break_glass": break_glass, "token_id": id }),
            );
        }
        ScimTokenIssue {
            id,
            account_id: account_id.to_string(),
            label: snapshot.label,
            secret,
            created_at: snapshot.created_at,
        }
    }

    pub fn revoke_token(
        &self,
        account_id: &str,
        token_id: &str,
        actor: &str,
        break_glass: bool,
    ) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap();
        let record = inner
            .tokens
            .get_mut(token_id)
            .ok_or_else(|| format!("unknown SCIM token: {token_id}"))?;
        if record.snapshot.account_id != account_id {
            return Err("SCIM token account mismatch".to_string());
        }
        let now = chrono::Utc::now().to_rfc3339();
        record.snapshot.revoked_at = Some(now.clone());
        inner.audit.push(json!({
            "action": "scim.token.revoke",
            "account_id": account_id,
            "actor": actor,
            "break_glass": break_glass,
            "token_id": token_id,
            "at": now,
        }));
        if let Some(catalog) = self.catalog() {
            match catalog.revoke_scim_token(account_id, token_id, &now) {
                Ok(true) => self.append_audit(
                    account_id,
                    "scim.token.revoke",
                    Some(actor),
                    "scim_token",
                    token_id,
                    json!({ "break_glass": break_glass, "token_id": token_id }),
                ),
                Ok(false) => return Err(format!("unknown SCIM token: {token_id}")),
                Err(e) => warn!("failed to persist SCIM token revoke: {e}"),
            }
        }
        Ok(())
    }

    pub fn list_tokens(&self, account_id: &str) -> Vec<ScimTokenSnapshot> {
        if let Some(catalog) = self.catalog() {
            match catalog.list_scim_tokens(account_id) {
                Ok(tokens) => {
                    return tokens
                        .into_iter()
                        .map(|record| ScimTokenSnapshot {
                            id: record.id,
                            account_id: record.account_id,
                            label: record.label,
                            created_at: record.created_at,
                            revoked_at: record.revoked_at,
                        })
                        .collect();
                }
                Err(e) => warn!("failed to list durable SCIM tokens: {e}"),
            }
        }
        let inner = self.inner.lock().unwrap();
        let mut tokens = inner
            .tokens
            .values()
            .filter(|record| record.snapshot.account_id == account_id)
            .map(|record| record.snapshot.clone())
            .collect::<Vec<_>>();
        tokens.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        tokens
    }

    pub fn authenticate_token(&self, secret: &str) -> Option<ScimTokenSnapshot> {
        if let Some(catalog) = self.catalog() {
            match catalog.all_scim_tokens() {
                Ok(tokens) => {
                    return tokens.into_iter().find_map(|record| {
                        if record.revoked_at.is_some() {
                            return None;
                        }
                        PasswordHash::new(&record.secret_hash)
                            .ok()
                            .and_then(|parsed| {
                                Argon2::default()
                                    .verify_password(secret.as_bytes(), &parsed)
                                    .ok()
                            })
                            .map(|_| ScimTokenSnapshot {
                                id: record.id,
                                account_id: record.account_id,
                                label: record.label,
                                created_at: record.created_at,
                                revoked_at: record.revoked_at,
                            })
                    });
                }
                Err(e) => warn!("failed to authenticate durable SCIM token: {e}"),
            }
        }
        let inner = self.inner.lock().unwrap();
        inner.tokens.values().find_map(|record| {
            if record.snapshot.revoked_at.is_some() {
                return None;
            }
            PasswordHash::new(&record.secret_hash)
                .ok()
                .and_then(|parsed| {
                    Argon2::default()
                        .verify_password(secret.as_bytes(), &parsed)
                        .ok()
                })
                .map(|_| record.snapshot.clone())
        })
    }

    pub fn upsert_user(
        &self,
        account_id: &str,
        id: Option<&str>,
        payload: &Value,
    ) -> ScimUserRecord {
        if let Some(catalog) = self.catalog() {
            let now = chrono::Utc::now().to_rfc3339();
            let requested_user_name = payload
                .get("userName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let existing = id
                .and_then(|id| catalog.get_scim_user(account_id, id).ok().flatten())
                .or_else(|| {
                    (!requested_user_name.is_empty())
                        .then(|| {
                            catalog
                                .get_scim_user_by_name(account_id, &requested_user_name)
                                .ok()
                                .flatten()
                        })
                        .flatten()
                });
            let id = id
                .map(ToOwned::to_owned)
                .or_else(|| existing.as_ref().map(|u| u.id.clone()))
                .or_else(|| catalog.next_scim_user_id(account_id).ok())
                .unwrap_or_else(|| format!("usr_{}_{}", account_id, now));
            let active = payload
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            let display_name = payload
                .get("displayName")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| existing.as_ref().and_then(|u| u.display_name.clone()));
            let user_name = if requested_user_name.is_empty() {
                existing
                    .as_ref()
                    .map(|u| u.user_name.clone())
                    .unwrap_or_else(|| id.clone())
            } else {
                requested_user_name
            };
            let created_at = existing
                .as_ref()
                .map(|u| u.created_at.clone())
                .unwrap_or_else(|| now.clone());
            let record = ScimUserRecord {
                id: id.clone(),
                account_id: account_id.to_string(),
                user_name: user_name.clone(),
                display_name,
                active,
                lifecycle: if active { "active" } else { "deactivated" }.to_string(),
                external_id: payload
                    .get("externalId")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                created_at,
                updated_at: now.clone(),
            };
            if let Err(e) = catalog.upsert_scim_user(&opensnow_catalog::ScimUserRecord {
                id: record.id.clone(),
                account_id: record.account_id.clone(),
                user_name: record.user_name.clone(),
                display_name: record.display_name.clone(),
                active: record.active,
                lifecycle: record.lifecycle.clone(),
                external_id: record.external_id.clone(),
                created_at: record.created_at.clone(),
                updated_at: record.updated_at.clone(),
            }) {
                warn!("failed to persist SCIM user: {e}");
            }
            self.append_audit(
                account_id,
                if active {
                    "scim.user.upsert"
                } else {
                    "scim.user.deactivate"
                },
                None,
                "scim_user",
                &id,
                json!({ "user_id": id, "user_name": user_name }),
            );
            return record;
        }
        let mut inner = self.inner.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let user_name = payload
            .get("userName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let id = id
            .map(ToOwned::to_owned)
            .or_else(|| {
                inner
                    .users_by_name
                    .get(&(account_id.to_string(), user_name.clone()))
                    .cloned()
            })
            .unwrap_or_else(|| {
                inner.user_counter += 1;
                format!("usr_{}_{}", account_id, inner.user_counter)
            });
        let active = payload
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let existing = inner
            .users
            .get(&(account_id.to_string(), id.clone()))
            .cloned();
        let display_name = payload
            .get("displayName")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| existing.as_ref().and_then(|u| u.display_name.clone()));
        let user_name = if user_name.is_empty() {
            existing
                .as_ref()
                .map(|u| u.user_name.clone())
                .unwrap_or_else(|| id.clone())
        } else {
            user_name
        };
        let created_at = existing
            .as_ref()
            .map(|u| u.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let record = ScimUserRecord {
            id: id.clone(),
            account_id: account_id.to_string(),
            user_name: user_name.clone(),
            display_name,
            active,
            lifecycle: if active { "active" } else { "deactivated" }.to_string(),
            external_id: payload
                .get("externalId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            created_at,
            updated_at: now.clone(),
        };
        inner
            .users_by_name
            .insert((account_id.to_string(), user_name), id.clone());
        inner
            .users
            .insert((account_id.to_string(), id.clone()), record.clone());
        inner.audit.push(json!({
            "action": if active { "scim.user.upsert" } else { "scim.user.deactivate" },
            "account_id": account_id,
            "user_id": id,
            "at": now,
        }));
        record
    }

    pub fn set_user_active(
        &self,
        account_id: &str,
        user_id: &str,
        active: bool,
    ) -> Option<ScimUserRecord> {
        if let Some(catalog) = self.catalog() {
            let mut record = catalog.get_scim_user(account_id, user_id).ok().flatten()?;
            let now = chrono::Utc::now().to_rfc3339();
            record.active = active;
            record.lifecycle = if active { "active" } else { "deactivated" }.to_string();
            record.updated_at = now;
            if let Err(e) = catalog.upsert_scim_user(&opensnow_catalog::ScimUserRecord {
                id: record.id.clone(),
                account_id: record.account_id.clone(),
                user_name: record.user_name.clone(),
                display_name: record.display_name.clone(),
                active: record.active,
                lifecycle: record.lifecycle.clone(),
                external_id: record.external_id.clone(),
                created_at: record.created_at.clone(),
                updated_at: record.updated_at.clone(),
            }) {
                warn!("failed to persist SCIM user lifecycle: {e}");
            }
            self.append_audit(
                account_id,
                if active {
                    "scim.user.activate"
                } else {
                    "scim.user.deactivate"
                },
                None,
                "scim_user",
                user_id,
                json!({ "user_id": user_id }),
            );
            return Some(ScimUserRecord {
                id: record.id,
                account_id: record.account_id,
                user_name: record.user_name,
                display_name: record.display_name,
                active: record.active,
                lifecycle: record.lifecycle,
                external_id: record.external_id,
                created_at: record.created_at,
                updated_at: record.updated_at,
            });
        }
        let mut inner = self.inner.lock().unwrap();
        let key = (account_id.to_string(), user_id.to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let user = inner.users.get_mut(&key)?;
        user.active = active;
        user.lifecycle = if active { "active" } else { "deactivated" }.to_string();
        user.updated_at = now.clone();
        let user = user.clone();
        inner.audit.push(json!({
            "action": if active { "scim.user.activate" } else { "scim.user.deactivate" },
            "account_id": account_id,
            "user_id": user_id,
            "at": now,
        }));
        Some(user)
    }

    pub fn user_snapshot(&self, account_id: &str, user_id: &str) -> Option<ScimUserRecord> {
        if let Some(catalog) = self.catalog() {
            return catalog
                .get_scim_user(account_id, user_id)
                .ok()
                .flatten()
                .map(|u| ScimUserRecord {
                    id: u.id,
                    account_id: u.account_id,
                    user_name: u.user_name,
                    display_name: u.display_name,
                    active: u.active,
                    lifecycle: u.lifecycle,
                    external_id: u.external_id,
                    created_at: u.created_at,
                    updated_at: u.updated_at,
                });
        }
        self.inner
            .lock()
            .unwrap()
            .users
            .get(&(account_id.to_string(), user_id.to_string()))
            .cloned()
    }

    pub fn list_users(
        &self,
        account_id: &str,
        user_name_filter: Option<&str>,
    ) -> Vec<ScimUserRecord> {
        if let Some(catalog) = self.catalog() {
            match catalog.list_scim_users(account_id, user_name_filter) {
                Ok(users) => {
                    return users
                        .into_iter()
                        .map(|u| ScimUserRecord {
                            id: u.id,
                            account_id: u.account_id,
                            user_name: u.user_name,
                            display_name: u.display_name,
                            active: u.active,
                            lifecycle: u.lifecycle,
                            external_id: u.external_id,
                            created_at: u.created_at,
                            updated_at: u.updated_at,
                        })
                        .collect();
                }
                Err(e) => warn!("failed to list durable SCIM users: {e}"),
            }
        }
        let filter = user_name_filter.map(|s| s.to_ascii_lowercase());
        let mut users: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .users
            .values()
            .filter(|u| u.account_id == account_id)
            .filter(|u| filter.as_ref().is_none_or(|f| u.user_name == *f))
            .cloned()
            .collect();
        users.sort_by(|a, b| a.user_name.cmp(&b.user_name));
        users
    }

    pub fn upsert_group(
        &self,
        account_id: &str,
        id: Option<&str>,
        payload: &Value,
    ) -> ScimGroupRecord {
        if let Some(catalog) = self.catalog() {
            let now = chrono::Utc::now().to_rfc3339();
            let display_name = payload
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or("SCIM")
                .to_string();
            let role = normalize_scim_role(&display_name);
            let existing = id.and_then(|id| catalog.get_scim_group(account_id, id).ok().flatten());
            let id = id
                .map(ToOwned::to_owned)
                .or_else(|| catalog.next_scim_group_id(account_id).ok())
                .unwrap_or_else(|| format!("grp_{}_{}", account_id, now));
            let members = payload
                .get("members")
                .and_then(Value::as_array)
                .map(|members| {
                    members
                        .iter()
                        .filter_map(|m| {
                            m.get("value")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| {
                    existing
                        .as_ref()
                        .map(|g| g.members.clone())
                        .unwrap_or_default()
                });
            let created_at = existing
                .as_ref()
                .map(|g| g.created_at.clone())
                .unwrap_or_else(|| now.clone());
            let record = ScimGroupRecord {
                id: id.clone(),
                account_id: account_id.to_string(),
                display_name,
                role,
                members,
                tombstoned: false,
                created_at,
                updated_at: now,
            };
            if let Err(e) = catalog.upsert_scim_group(&opensnow_catalog::ScimGroupRecord {
                id: record.id.clone(),
                account_id: record.account_id.clone(),
                display_name: record.display_name.clone(),
                role: record.role.clone(),
                members: record.members.clone(),
                tombstoned: record.tombstoned,
                created_at: record.created_at.clone(),
                updated_at: record.updated_at.clone(),
            }) {
                warn!("failed to persist SCIM group: {e}");
            }
            self.append_audit(
                account_id,
                "scim.group.upsert",
                None,
                "scim_group",
                &id,
                json!({ "group_id": id, "role": record.role }),
            );
            return record;
        }
        let mut inner = self.inner.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let display_name = payload
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or("SCIM")
            .to_string();
        let role = normalize_scim_role(&display_name);
        let id = id.map(ToOwned::to_owned).unwrap_or_else(|| {
            inner.group_counter += 1;
            format!("grp_{}_{}", account_id, inner.group_counter)
        });
        let existing = inner
            .groups
            .get(&(account_id.to_string(), id.clone()))
            .cloned();
        let members = payload
            .get("members")
            .and_then(Value::as_array)
            .map(|members| {
                members
                    .iter()
                    .filter_map(|m| {
                        m.get("value")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                existing
                    .as_ref()
                    .map(|g| g.members.clone())
                    .unwrap_or_default()
            });
        let created_at = existing
            .as_ref()
            .map(|g| g.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let record = ScimGroupRecord {
            id: id.clone(),
            account_id: account_id.to_string(),
            display_name,
            role,
            members,
            tombstoned: false,
            created_at,
            updated_at: now.clone(),
        };
        inner
            .groups
            .insert((account_id.to_string(), id.clone()), record.clone());
        inner.audit.push(json!({
            "action": "scim.group.upsert",
            "account_id": account_id,
            "group_id": id,
            "role": record.role,
            "at": now,
        }));
        record
    }

    pub fn tombstone_group(&self, account_id: &str, group_id: &str) -> Option<ScimGroupRecord> {
        if let Some(catalog) = self.catalog() {
            let mut record = catalog
                .get_scim_group(account_id, group_id)
                .ok()
                .flatten()?;
            record.tombstoned = true;
            record.members.clear();
            record.updated_at = chrono::Utc::now().to_rfc3339();
            if let Err(e) = catalog.upsert_scim_group(&opensnow_catalog::ScimGroupRecord {
                id: record.id.clone(),
                account_id: record.account_id.clone(),
                display_name: record.display_name.clone(),
                role: record.role.clone(),
                members: record.members.clone(),
                tombstoned: record.tombstoned,
                created_at: record.created_at.clone(),
                updated_at: record.updated_at.clone(),
            }) {
                warn!("failed to persist SCIM group tombstone: {e}");
            }
            self.append_audit(
                account_id,
                "scim.group.tombstone",
                None,
                "scim_group",
                group_id,
                json!({ "group_id": group_id }),
            );
            return Some(ScimGroupRecord {
                id: record.id,
                account_id: record.account_id,
                display_name: record.display_name,
                role: record.role,
                members: record.members,
                tombstoned: record.tombstoned,
                created_at: record.created_at,
                updated_at: record.updated_at,
            });
        }
        let mut inner = self.inner.lock().unwrap();
        let key = (account_id.to_string(), group_id.to_string());
        let now = chrono::Utc::now().to_rfc3339();
        let group = inner.groups.get_mut(&key)?;
        group.tombstoned = true;
        group.members.clear();
        group.updated_at = now.clone();
        let group = group.clone();
        inner.audit.push(json!({
            "action": "scim.group.tombstone",
            "account_id": account_id,
            "group_id": group_id,
            "at": now,
        }));
        Some(group)
    }

    pub fn group_snapshot(&self, account_id: &str, group_id: &str) -> Option<ScimGroupRecord> {
        if let Some(catalog) = self.catalog() {
            return catalog
                .get_scim_group(account_id, group_id)
                .ok()
                .flatten()
                .map(|g| ScimGroupRecord {
                    id: g.id,
                    account_id: g.account_id,
                    display_name: g.display_name,
                    role: g.role,
                    members: g.members,
                    tombstoned: g.tombstoned,
                    created_at: g.created_at,
                    updated_at: g.updated_at,
                });
        }
        self.inner
            .lock()
            .unwrap()
            .groups
            .get(&(account_id.to_string(), group_id.to_string()))
            .cloned()
    }

    pub fn list_groups(&self, account_id: &str) -> Vec<ScimGroupRecord> {
        if let Some(catalog) = self.catalog() {
            match catalog.list_scim_groups(account_id) {
                Ok(groups) => {
                    return groups
                        .into_iter()
                        .map(|g| ScimGroupRecord {
                            id: g.id,
                            account_id: g.account_id,
                            display_name: g.display_name,
                            role: g.role,
                            members: g.members,
                            tombstoned: g.tombstoned,
                            created_at: g.created_at,
                            updated_at: g.updated_at,
                        })
                        .collect();
                }
                Err(e) => warn!("failed to list durable SCIM groups: {e}"),
            }
        }
        let mut groups: Vec<_> = self
            .inner
            .lock()
            .unwrap()
            .groups
            .values()
            .filter(|g| g.account_id == account_id && !g.tombstoned)
            .cloned()
            .collect();
        groups.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        groups
    }

    pub fn audit_events(&self) -> Vec<Value> {
        self.inner.lock().unwrap().audit.clone()
    }
}

fn normalize_scim_role(display_name: &str) -> String {
    match display_name.trim().to_ascii_uppercase().as_str() {
        "ACCOUNTOWNER" | "ACCOUNT_OWNER" | "OWNER" => "ACCOUNTOWNER".to_string(),
        "ACCOUNTADMIN" | "ACCOUNT_ADMIN" | "ADMIN" => "ACCOUNTADMIN".to_string(),
        "SYSADMIN" => "SYSADMIN".to_string(),
        "ANALYST" => "ANALYST".to_string(),
        other => other.replace([' ', '-'], "_").to_ascii_uppercase(),
    }
}

/// Authenticated request context installed by [`jwt_required`]. Handlers use
/// this instead of trusting caller-supplied tenant headers.
#[derive(Clone, Debug)]
pub struct AuthContext {
    pub user_id: i64,
    pub username: String,
    pub role: String,
    pub tenant_id: String,
    pub scopes: Vec<String>,
}

impl From<Claims> for AuthContext {
    fn from(claims: Claims) -> Self {
        Self {
            user_id: claims.user_id,
            username: claims.username,
            role: claims.role,
            tenant_id: claims.tenant_id,
            scopes: claims.scopes,
        }
    }
}

impl AuthContext {
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }

    pub fn has_explicit_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    pub fn has_any_scope(&self, scopes: &[&str]) -> bool {
        scopes.iter().any(|scope| self.has_scope(scope))
    }

    pub fn has_all_scopes(&self, scopes: &[&str]) -> bool {
        scopes.iter().all(|scope| self.has_scope(scope))
    }

    pub fn is_platform_admin(&self) -> bool {
        matches!(self.role.as_str(), "ACCOUNTADMIN" | "SYSADMIN")
            || self.has_any_scope(&["policy.admin", "admin"])
    }
}

fn authorize(
    req: &Request<Body>,
    all_scopes: &[&str],
    any_scopes: &[&str],
) -> Result<(), StatusCode> {
    let auth = req
        .extensions()
        .get::<AuthContext>()
        .ok_or(StatusCode::UNAUTHORIZED)?;

    if auth.is_platform_admin()
        || (!all_scopes.is_empty() && auth.has_all_scopes(all_scopes))
        || (!any_scopes.is_empty() && auth.has_any_scope(any_scopes))
    {
        Ok(())
    } else {
        warn!(
            user = %auth.username,
            role = %auth.role,
            tenant = %auth.tenant_id,
            required_all = ?all_scopes,
            required_any = ?any_scopes,
            "forbidden by route authorization policy"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

pub async fn require_query_scope(req: Request<Body>, next: Next) -> Result<Response, StatusCode> {
    authorize(&req, &["sql.query", "table.select"], &[])?;
    Ok(next.run(req).await)
}

pub async fn require_ingest_write_scope(
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize(&req, &[], &["ingest.write", "table.insert", "table.create"])?;
    Ok(next.run(req).await)
}

pub async fn require_ingest_read_scope(
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize(&req, &[], &["ingest.read", "ingest.write"])?;
    Ok(next.run(req).await)
}

pub async fn require_admin_scope(req: Request<Body>, next: Next) -> Result<Response, StatusCode> {
    authorize(&req, &[], &["policy.admin"])?;
    Ok(next.run(req).await)
}

pub async fn require_audit_read_scope(
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    authorize(&req, &[], &["audit.read", "policy.admin"])?;
    Ok(next.run(req).await)
}

impl<S> FromRequestParts<S> for AuthContext
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or((StatusCode::UNAUTHORIZED, "missing auth context"))
    }
}

#[derive(Debug, Clone)]
struct ClientConfigEntry {
    client_id: String,
    secret: String,
    role: String,
    tenant_id: String,
    scopes: Vec<String>,
}

fn parse_client_entry(entry: &str) -> Option<ClientConfigEntry> {
    let parts: Vec<_> = entry.splitn(5, ':').map(str::trim).collect();
    let client_id = *parts.first()?;
    let secret = *parts.get(1)?;
    let role = *parts.get(2)?;
    if client_id.is_empty() || secret.is_empty() || role.is_empty() {
        return None;
    }

    let tenant_id = parts
        .get(3)
        .filter(|s| !s.is_empty())
        .copied()
        .unwrap_or("default");
    let scopes = parts
        .get(4)
        .map(|raw| {
            raw.split_whitespace()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|scopes| !scopes.is_empty())
        .unwrap_or_else(|| vec![role.to_string()]);

    Some(ClientConfigEntry {
        client_id: client_id.to_string(),
        secret: secret.to_string(),
        role: role.to_string(),
        tenant_id: tenant_id.to_string(),
        scopes,
    })
}

#[derive(Debug, Deserialize)]
struct RotatedJwtKeyEnv {
    kid: String,
    #[serde(default = "default_jwt_alg")]
    alg: String,
    public_key_pem: String,
    #[serde(default)]
    jwk: Option<JsonWebKey>,
}

fn default_jwt_alg() -> String {
    "RS256".to_string()
}

fn jwt_algorithm_from_env(value: &str) -> jsonwebtoken::errors::Result<jsonwebtoken::Algorithm> {
    match value.trim().to_ascii_uppercase().as_str() {
        "RS256" => Ok(jsonwebtoken::Algorithm::RS256),
        "ES256" => Ok(jsonwebtoken::Algorithm::ES256),
        _ => Err(jsonwebtoken::errors::Error::from(
            jsonwebtoken::errors::ErrorKind::InvalidAlgorithm,
        )),
    }
}

fn read_env_or_file(value_var: &str, path_var: &str) -> Result<String, String> {
    if let Ok(value) = std::env::var(value_var) {
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }
    let path = std::env::var(path_var)
        .map_err(|_| format!("{value_var} or {path_var} is required for enterprise JWT mode"))?;
    std::fs::read_to_string(&path).map_err(|e| format!("failed to read {path_var} {path}: {e}"))
}

fn enterprise_jwk_from_env(kid: &str, alg: jsonwebtoken::Algorithm) -> Option<JsonWebKey> {
    match alg {
        jsonwebtoken::Algorithm::RS256 => {
            let n = std::env::var("OPENSNOW_JWT_JWK_N").ok()?;
            let e = std::env::var("OPENSNOW_JWT_JWK_E").unwrap_or_else(|_| "AQAB".to_string());
            Some(JsonWebKey::rsa(kid, &n, &e))
        }
        jsonwebtoken::Algorithm::ES256 => {
            let x = std::env::var("OPENSNOW_JWT_JWK_X").ok()?;
            let y = std::env::var("OPENSNOW_JWT_JWK_Y").ok()?;
            Some(JsonWebKey::ec_p256(kid, &x, &y))
        }
        _ => None,
    }
}

fn jwt_manager_from_env() -> Option<Arc<JwtManager>> {
    let mode = std::env::var("OPENSNOW_JWT_MODE")
        .unwrap_or_else(|_| "local_hs256".to_string())
        .to_ascii_lowercase();

    if mode == "enterprise" || mode == "rs256" || mode == "es256" {
        let issuer = std::env::var("OPENSNOW_JWT_ISSUER")
            .unwrap_or_else(|_| panic!("OPENSNOW_JWT_ISSUER is required for enterprise JWT mode"));
        let audience = std::env::var("OPENSNOW_JWT_AUDIENCE").unwrap_or_else(|_| {
            panic!("OPENSNOW_JWT_AUDIENCE is required for enterprise JWT mode")
        });
        let kid = std::env::var("OPENSNOW_JWT_KID")
            .unwrap_or_else(|_| panic!("OPENSNOW_JWT_KID is required for enterprise JWT mode"));
        let alg = jwt_algorithm_from_env(&std::env::var("OPENSNOW_JWT_ALGORITHM").unwrap_or_else(
            |_| {
                if mode == "es256" {
                    "ES256".to_string()
                } else {
                    "RS256".to_string()
                }
            },
        ))
        .unwrap_or_else(|_| panic!("OPENSNOW_JWT_ALGORITHM must be RS256 or ES256"));
        let private_key = read_env_or_file(
            "OPENSNOW_JWT_PRIVATE_KEY_PEM",
            "OPENSNOW_JWT_PRIVATE_KEY_PATH",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let public_key = read_env_or_file(
            "OPENSNOW_JWT_PUBLIC_KEY_PEM",
            "OPENSNOW_JWT_PUBLIC_KEY_PATH",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let active_key = EnterpriseJwtKey::from_pem(
            &kid,
            alg,
            private_key.as_bytes(),
            public_key.as_bytes(),
            enterprise_jwk_from_env(&kid, alg),
        )
        .unwrap_or_else(|e| panic!("invalid enterprise JWT active key: {e}"));
        let verification_keys = std::env::var("OPENSNOW_JWT_VERIFICATION_KEYS_JSON")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|json| {
                serde_json::from_str::<Vec<RotatedJwtKeyEnv>>(&json)
                    .unwrap_or_else(|e| panic!("invalid OPENSNOW_JWT_VERIFICATION_KEYS_JSON: {e}"))
                    .into_iter()
                    .map(|key| {
                        let alg = jwt_algorithm_from_env(&key.alg).unwrap_or_else(|_| {
                            panic!("rotated JWT key {} has unsupported alg", key.kid)
                        });
                        EnterpriseJwtKey::verify_only_from_pem(
                            &key.kid,
                            alg,
                            key.public_key_pem.as_bytes(),
                            key.jwk,
                        )
                        .unwrap_or_else(|e| panic!("invalid rotated JWT key {}: {e}", key.kid))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let revoked_kids = std::env::var("OPENSNOW_JWT_REVOKED_KIDS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|kid| !kid.is_empty())
            .map(ToString::to_string)
            .collect();
        return Some(Arc::new(
            JwtManager::enterprise(EnterpriseJwtConfig {
                issuer,
                audience,
                active_key,
                verification_keys,
                revoked_kids,
            })
            .unwrap_or_else(|e| panic!("invalid enterprise JWT config: {e}")),
        ));
    }

    let secret = std::env::var("OPENSNOW_JWT_SECRET").ok()?;
    if secret.is_empty() {
        None
    } else {
        Some(Arc::new(JwtManager::new(secret.as_bytes())))
    }
}

/// Shared state for the auth middleware + token endpoint.
#[derive(Clone)]
pub struct AuthState {
    pub jwt: Arc<JwtManager>,
    pub clients: ClientRegistry,
    pub scim: ScimDirectory,
    pub token_expiry_hours: i64,
    pub durable_service_catalog_path: Option<String>,
    pub sso_session_store_path: Option<String>,
    pub policy: crate::policy::ObjectPolicyStore,
}

impl AuthState {
    pub fn new(jwt: Arc<JwtManager>, clients: ClientRegistry, token_expiry_hours: i64) -> Self {
        Self {
            jwt,
            clients,
            scim: ScimDirectory::new(),
            token_expiry_hours,
            durable_service_catalog_path: None,
            sso_session_store_path: None,
            policy: crate::policy::ObjectPolicyStore::in_memory()
                .expect("in-memory policy store initializes"),
        }
    }

    pub fn with_durable_service_catalog_path(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        self.scim = ScimDirectory::durable(path.clone());
        self.durable_service_catalog_path = Some(path);
        self
    }

    pub fn with_sso_session_store(mut self, path: impl Into<String>) -> Self {
        self.sso_session_store_path = Some(path.into());
        self
    }

    fn catalog(&self) -> Result<Option<Catalog>, String> {
        self.durable_service_catalog_path
            .as_deref()
            .map(|path| Catalog::open(path).map_err(|e| e.to_string()))
            .transpose()
    }

    /// Build an `AuthState` from environment variables, or return `None` if
    /// no JWT secret is configured (auth-disabled mode).
    ///
    /// Reads:
    /// - `OPENSNOW_JWT_MODE=enterprise` plus `OPENSNOW_JWT_ISSUER`,
    ///   `OPENSNOW_JWT_AUDIENCE`, `OPENSNOW_JWT_KID`, RS256/ES256 PEM key
    ///   env/path values, JWK coordinates, optional rotated verify-only keys,
    ///   and optional `OPENSNOW_JWT_REVOKED_KIDS` for production asymmetric auth.
    /// - `OPENSNOW_JWT_SECRET` — HMAC secret for local/dev auth.
    /// - `OPENSNOW_CLIENTS` — comma-separated `id:secret:role[:tenant_id[:scope scope]]`.
    /// - `OPENSNOW_TOKEN_EXPIRY_HOURS` — optional, default 24.
    pub fn from_env() -> Option<Self> {
        let jwt = jwt_manager_from_env()?;
        let clients = ClientRegistry::new();
        for entry in std::env::var("OPENSNOW_CLIENTS")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match parse_client_entry(entry) {
                Some(parsed) => clients.register_with_metadata(
                    &parsed.client_id,
                    &parsed.secret,
                    &parsed.role,
                    &parsed.tenant_id,
                    parsed.scopes,
                ),
                None => warn!("ignoring malformed OPENSNOW_CLIENTS entry: {entry}"),
            }
        }
        let catalog_path = std::env::var("OPENSNOW_AUTH_CATALOG_PATH")
            .ok()
            .or_else(|| std::env::var("OPENSNOW_CATALOG_PATH").ok())
            .filter(|path| !path.trim().is_empty());
        if let Some(path) = catalog_path.as_deref() {
            if let Err(e) = clients.load_durable_clients_from_catalog(path) {
                warn!("failed to load durable service clients from catalog: {e}");
            }
        }
        let expiry = std::env::var("OPENSNOW_TOKEN_EXPIRY_HOURS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(24);
        let mut state = Self::new(jwt, clients, expiry);
        if let Some(path) = catalog_path.as_deref() {
            match crate::policy::ObjectPolicyStore::from_catalog_path(path) {
                Ok(policy) => state.policy = policy,
                Err(e) => warn!("failed to load durable object policy store from catalog: {e}"),
            }
        }
        if let Some(path) = catalog_path.as_deref() {
            state.scim = ScimDirectory::durable(path.to_string());
        }
        state.durable_service_catalog_path = catalog_path;
        state.sso_session_store_path = std::env::var("OPENSNOW_SSO_DB_PATH")
            .ok()
            .filter(|path| !path.trim().is_empty());
        Some(state)
    }
}

/// `Authorization: Bearer *** → reject 401 on missing/invalid token.
pub async fn jwt_required(
    State(state): State<AuthState>,
    mut req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim());

    match token {
        Some(t) => match state.jwt.validate_token(t) {
            Ok(claims) => {
                if claims.auth_method.as_deref() == Some("oidc") {
                    let Some(session_id) = claims.session_id.as_deref() else {
                        warn!(user = %claims.username, "rejected OIDC JWT without durable session id");
                        return Err(StatusCode::UNAUTHORIZED);
                    };
                    let Some(sso_path) = state.sso_session_store_path.as_deref() else {
                        warn!(user = %claims.username, "rejected OIDC JWT because OPENSNOW_SSO_DB_PATH is not configured");
                        return Err(StatusCode::UNAUTHORIZED);
                    };
                    let sso = match opensnow_auth::SsoManager::open(sso_path) {
                        Ok(manager) => manager,
                        Err(e) => {
                            warn!("failed to open SSO session store: {e}");
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                    };
                    if let Err(e) =
                        sso.validate_sso_session(session_id, &claims.tenant_id, &claims.username)
                    {
                        warn!(user = %claims.username, session_id, "rejected OIDC JWT with invalid durable session: {e}");
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                } else {
                    if let Some(catalog_path) = state.durable_service_catalog_path.as_deref() {
                        if let Err(e) = state
                            .clients
                            .refresh_durable_client_from_catalog(catalog_path, &claims.username)
                        {
                            warn!(client_id = %claims.username, "failed to refresh durable service client state: {e}");
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                    }
                    state
                        .clients
                        .authorize_bearer_client(&claims.username, &claims.scopes)?;
                }

                for account_header in ["X-Tenant-ID", "X-Account-ID"] {
                    if let Some(requested_account) = req
                        .headers()
                        .get(account_header)
                        .and_then(|v| v.to_str().ok())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    {
                        if requested_account != claims.tenant_id {
                            warn!(
                                user = %claims.username,
                                token_tenant = %claims.tenant_id,
                                requested_account,
                                header = account_header,
                                "rejected account/tenant spoofing attempt"
                            );
                            return Err(StatusCode::FORBIDDEN);
                        }
                    }
                }

                debug!(user = %claims.username, role = %claims.role, tenant = %claims.tenant_id, "authenticated");
                req.extensions_mut()
                    .insert(crate::tenant::TenantId(claims.tenant_id.clone()));
                req.extensions_mut().insert(state.policy.clone());
                req.extensions_mut().insert(AuthContext::from(claims));
                Ok(next.run(req).await)
            }
            Err(e) => {
                warn!("rejected JWT: {e}");
                Err(StatusCode::UNAUTHORIZED)
            }
        },
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

/// OAuth2 `client_credentials` request body. Accept JSON for simplicity
/// (form-encoding can be added later if a strict OAuth2 client requires it).
#[derive(Debug, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub client_id: String,
    pub client_secret: String,
}

/// OAuth2 token response. Always uses Bearer; we don't issue refresh tokens
/// because client_credentials does not need them.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub scope: String,
}

/// `POST /auth/token` — the OAuth2 client_credentials grant.
pub async fn token_endpoint(
    State(state): State<AuthState>,
    Json(req): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, (StatusCode, Json<Value>)> {
    if req.grant_type != "client_credentials" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "unsupported_grant_type",
                "error_description": format!("only client_credentials supported, got {}", req.grant_type),
            })),
        ));
    }

    let client = state
        .clients
        .authenticate_client(&req.client_id, &req.client_secret)
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({
                    "error": "invalid_client",
                    "error_description": "unknown client_id or wrong secret",
                })),
            )
        })?;

    if let Some(catalog) = state.catalog().map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": "server_error",
                "error_description": message,
            })),
        )
    })? {
        if catalog
            .get_service_identity_client(&req.client_id)
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "server_error",
                        "error_description": e.to_string(),
                    })),
                )
            })?
            .is_some()
        {
            catalog
                .mark_service_identity_used(&req.client_id)
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({
                            "error": "server_error",
                            "error_description": e.to_string(),
                        })),
                    )
                })?;
        }
    }

    // user_id 0 is reserved for "machine client" — JWT carries the
    // client_id as the username so logs / spans can attribute usage.
    let token = state
        .jwt
        .generate_token_with_scopes(
            0,
            &req.client_id,
            &client.role,
            &client.tenant_id,
            client.scopes.clone(),
            state.token_expiry_hours,
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "error": "server_error",
                    "error_description": e.to_string(),
                })),
            )
        })?;

    Ok(Json(TokenResponse {
        access_token: token,
        token_type: "Bearer".to_string(),
        expires_in: state.token_expiry_hours * 3600,
        scope: client.scopes.join(" "),
    }))
}

pub async fn jwks_endpoint(
    State(state): State<AuthState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    state.jwt.jwks().map(Json).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "jwks_not_configured",
                "error_description": "JWKS is available only in enterprise RS256/ES256 JWT mode",
            })),
        )
    })
}

#[derive(Debug, Serialize)]
struct ServiceClientApiSnapshot {
    client_id: String,
    account_id: String,
    workspace_id: Option<String>,
    role: String,
    scopes: Vec<String>,
    status: String,
    status_reason: Option<String>,
    expires_at: Option<String>,
    last_used_at: Option<String>,
    rotated_at: Option<String>,
    revoked_at: Option<String>,
    created_at: String,
}

impl From<ServiceIdentityClient> for ServiceClientApiSnapshot {
    fn from(value: ServiceIdentityClient) -> Self {
        Self {
            client_id: value.id,
            account_id: value.account_id,
            workspace_id: value.workspace_id,
            role: value.role,
            scopes: value.scopes,
            status: value.status,
            status_reason: value.status_reason,
            expires_at: value.expires_at,
            last_used_at: value.last_used_at,
            rotated_at: value.rotated_at,
            revoked_at: value.revoked_at,
            created_at: value.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ServiceClientCreateRequest {
    client_id: String,
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    workspace_id: Option<String>,
    role: String,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ServiceClientRotateRequest {
    #[serde(default)]
    client_secret: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AccountCreateRequest {
    slug: String,
    #[serde(default)]
    legal_name: Option<String>,
    #[serde(default)]
    owner_email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrganizationCreateRequest {
    slug: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    verified_domains: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct WorkspaceCreateRequest {
    slug: String,
    #[serde(default)]
    deployment_mode: Option<String>,
    #[serde(default)]
    warehouse_namespace: Option<String>,
}

fn generated_client_secret() -> String {
    format!("osk_{}", SaltString::generate(&mut OsRng).as_str())
}

fn slug_id(prefix: &str, parent: &str, slug: &str) -> String {
    format!(
        "{prefix}_{parent}_{}",
        slug.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-")
    )
}

fn server_error(message: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "status": "error", "message": message.to_string() })),
    )
}

fn forbidden(message: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::FORBIDDEN,
        Json(json!({ "status": "error", "message": message.to_string() })),
    )
}

fn not_found(message: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "status": "error", "message": message.to_string() })),
    )
}

fn require_catalog(state: &AuthState) -> Result<Catalog, (StatusCode, Json<Value>)> {
    state.catalog().map_err(server_error)?.ok_or_else(|| {
        (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "status": "error", "message": "durable control-plane catalog is not configured" })))
    })
}

fn is_break_glass_admin(actor: &AuthContext) -> bool {
    actor.has_explicit_scope("break_glass.admin")
}

fn contains_plaintext_secret(value: &Value) -> Option<String> {
    fn normalized_secret_key(key: &str) -> String {
        let mut normalized = String::with_capacity(key.len());
        let mut previous_was_separator = true;
        let mut previous_was_lower_or_digit = false;

        for ch in key.chars() {
            if ch == '-' || ch == '_' || ch.is_whitespace() {
                if !previous_was_separator {
                    normalized.push('_');
                }
                previous_was_separator = true;
                previous_was_lower_or_digit = false;
            } else if ch.is_ascii_uppercase() {
                if previous_was_lower_or_digit && !previous_was_separator {
                    normalized.push('_');
                }
                normalized.push(ch.to_ascii_lowercase());
                previous_was_separator = false;
                previous_was_lower_or_digit = false;
            } else {
                normalized.push(ch.to_ascii_lowercase());
                previous_was_separator = false;
                previous_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
            }
        }

        normalized.trim_matches('_').to_string()
    }

    fn is_secret_key(key: &str) -> bool {
        let normalized = normalized_secret_key(key);
        if normalized.ends_with("_handle")
            || normalized.ends_with("_handle_ref")
            || normalized.ends_with("_handle_refs")
            || normalized == "handle"
            || normalized == "secret_handle"
            || normalized == "secret_handles"
        {
            return false;
        }
        matches!(
            normalized.as_str(),
            "client_secret"
                | "token"
                | "password"
                | "passwd"
                | "api_key"
                | "apikey"
                | "access_key"
                | "secret_key"
                | "secret_access_key"
                | "private_key"
                | "secret"
        ) || normalized.ends_with("_token")
            || normalized.ends_with("_password")
            || normalized.ends_with("_private_key")
            || normalized.ends_with("_access_key")
            || normalized.ends_with("_api_key")
            || normalized.ends_with("_apikey")
            || normalized.ends_with("_secret_key")
    }

    match value {
        Value::Object(map) => map.iter().find_map(|(key, nested)| {
            if is_secret_key(key) && !nested.is_null() {
                Some(key.clone())
            } else {
                contains_plaintext_secret(nested)
            }
        }),
        Value::Array(items) => items.iter().find_map(contains_plaintext_secret),
        _ => None,
    }
}

fn reject_plaintext_secret_payload(payload: &Value) -> Result<(), (StatusCode, Json<Value>)> {
    if let Some(key) = contains_plaintext_secret(payload) {
        Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "error",
                "message": format!("plaintext secret-bearing field `{key}` is not accepted; store it in a secret manager and pass a *_handle reference")
            })),
        ))
    } else {
        Ok(())
    }
}

fn ensure_account_actor(
    actor: &AuthContext,
    account_id: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    if actor.tenant_id == account_id || is_break_glass_admin(actor) {
        Ok(())
    } else {
        Err(forbidden(
            "authenticated subject cannot mutate a different account",
        ))
    }
}

fn audit_control_plane(
    catalog: &Catalog,
    actor: &AuthContext,
    account_id: &str,
    organization_id: Option<&str>,
    workspace_id: Option<&str>,
    action: &str,
    resource_type: &str,
    resource_id: &str,
    decision: AuditResult,
    metadata: Value,
) {
    let mut metadata_redacted = metadata.as_object().cloned().unwrap_or_default();
    if let Some(workspace_id) = workspace_id {
        metadata_redacted.insert(
            "workspace_id".to_string(),
            Value::String(workspace_id.to_string()),
        );
    }
    let event = AuditEvent {
        event_time: chrono::Utc::now(),
        organization_id: organization_id.unwrap_or(account_id).to_string(),
        tenant_id: Some(account_id.to_string()),
        actor_type: "user".to_string(),
        actor_id: actor.username.clone(),
        actor_display: Some(actor.username.clone()),
        actor_auth_method: Some("jwt".to_string()),
        action: action.to_string(),
        resource_type: resource_type.to_string(),
        resource_id: resource_id.to_string(),
        resource_name: None,
        result: decision,
        trace_id: None,
        secret_handle_refs: Vec::new(),
        metadata_redacted,
    };
    if let Err(e) = catalog.append_audit_event(account_id, organization_id, &event) {
        warn!("failed to append control-plane audit event: {e}");
    }
}

fn audit_denied_control_plane(
    catalog: &Catalog,
    actor: &AuthContext,
    account_id: &str,
    organization_id: Option<&str>,
    workspace_id: Option<&str>,
    action: &str,
    resource_type: &str,
    resource_id: &str,
) {
    audit_control_plane(
        catalog,
        actor,
        account_id,
        organization_id,
        workspace_id,
        action,
        resource_type,
        resource_id,
        AuditResult::Denied,
        json!({"decision":"denied", "reason":"cross_account_or_workspace"}),
    );
}

async fn create_account_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Json(req): Json<AccountCreateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !is_break_glass_admin(&actor) {
        return Err(forbidden(
            "account bootstrap requires platform break-glass/admin authority",
        ));
    }
    let catalog = require_catalog(&state)?;
    let owner_email = req
        .owner_email
        .unwrap_or_else(|| format!("owner@{}.example", req.slug));
    let bootstrap = catalog
        .register_account(&req.slug, &owner_email)
        .map_err(server_error)?;
    let mut account = serde_json::to_value(&bootstrap.account).map_err(server_error)?;
    account["legal_name"] = json!(req.legal_name.unwrap_or_else(|| req.slug.clone()));
    catalog
        .upsert_control_plane_resource(
            "account",
            &bootstrap.account.id,
            &bootstrap.account.id,
            Some(&bootstrap.organization.id),
            None,
            &account,
        )
        .map_err(server_error)?;
    catalog
        .upsert_control_plane_resource(
            "organization",
            &bootstrap.organization.id,
            &bootstrap.account.id,
            Some(&bootstrap.organization.id),
            None,
            &serde_json::to_value(&bootstrap.organization).map_err(server_error)?,
        )
        .map_err(server_error)?;
    catalog
        .upsert_control_plane_resource(
            "workspace",
            &bootstrap.workspace.id,
            &bootstrap.account.id,
            Some(&bootstrap.organization.id),
            Some(&bootstrap.workspace.id),
            &serde_json::to_value(&bootstrap.workspace).map_err(server_error)?,
        )
        .map_err(server_error)?;
    audit_control_plane(
        &catalog,
        &actor,
        &bootstrap.account.id,
        Some(&bootstrap.organization.id),
        None,
        "control_plane.account.create",
        "account",
        &bootstrap.account.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(
        json!({ "status": "ok", "account": bootstrap.account, "organization": bootstrap.organization, "workspace": bootstrap.workspace }),
    ))
}

async fn create_organization_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path(account_id): Path<String>,
    Json(req): Json<OrganizationCreateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    if let Err(e) = ensure_account_actor(&actor, &account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &account_id,
            None,
            None,
            "control_plane.organization.create",
            "organization",
            &account_id,
        );
        return Err(e);
    }
    let org_id = slug_id("org", &account_id, &req.slug);
    let resource = json!({"id": org_id, "account_id": account_id, "slug": req.slug, "display_name": req.display_name, "verified_domains": req.verified_domains, "lifecycle_state":"active"});
    let org = catalog
        .upsert_control_plane_resource(
            "organization",
            resource["id"].as_str().unwrap(),
            &account_id,
            Some(resource["id"].as_str().unwrap()),
            None,
            &resource,
        )
        .map_err(server_error)?;
    audit_control_plane(
        &catalog,
        &actor,
        &account_id,
        Some(&org.id),
        None,
        "control_plane.organization.create",
        "organization",
        &org.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(json!({"status":"ok", "organization": org.resource})))
}

fn organization_owner(
    catalog: &Catalog,
    org_id: &str,
) -> Result<opensnow_catalog::ControlPlaneResource, (StatusCode, Json<Value>)> {
    catalog
        .get_control_plane_resource("organization", org_id)
        .map_err(server_error)?
        .ok_or_else(|| not_found("organization not found"))
}

fn workspace_owner(
    catalog: &Catalog,
    workspace_id: &str,
) -> Result<opensnow_catalog::ControlPlaneResource, (StatusCode, Json<Value>)> {
    catalog
        .get_control_plane_resource("workspace", workspace_id)
        .map_err(server_error)?
        .ok_or_else(|| not_found("workspace not found"))
}

async fn create_workspace_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path(org_id): Path<String>,
    Json(req): Json<WorkspaceCreateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let org = organization_owner(&catalog, &org_id)?;
    if let Err(e) = ensure_account_actor(&actor, &org.account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &org.account_id,
            Some(&org_id),
            None,
            "control_plane.workspace.create",
            "workspace",
            &org_id,
        );
        return Err(e);
    }
    let workspace_id = slug_id("ws", &org.account_id, &req.slug);
    let resource = json!({"id": workspace_id, "account_id": org.account_id, "organization_id": org_id, "slug": req.slug, "deployment_mode": req.deployment_mode.unwrap_or_else(|| "self_hosted_k8s".to_string()), "warehouse_namespace": req.warehouse_namespace, "lifecycle_state":"active"});
    let workspace = catalog
        .upsert_control_plane_resource(
            "workspace",
            resource["id"].as_str().unwrap(),
            &org.account_id,
            Some(&org_id),
            Some(resource["id"].as_str().unwrap()),
            &resource,
        )
        .map_err(server_error)?;
    audit_control_plane(
        &catalog,
        &actor,
        &org.account_id,
        Some(&org_id),
        Some(&workspace.id),
        "control_plane.workspace.create",
        "workspace",
        &workspace.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(
        json!({"status":"ok", "workspace": workspace.resource}),
    ))
}

async fn create_org_resource_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path((org_id, kind)): Path<(String, String)>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let org = organization_owner(&catalog, &org_id)?;
    if let Err(e) = ensure_account_actor(&actor, &org.account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &org.account_id,
            Some(&org_id),
            None,
            &format!(
                "control_plane.{}.create",
                kind.trim_end_matches('s').replace('-', "_")
            ),
            &kind,
            &org_id,
        );
        return Err(e);
    }
    let (resource_type, response_key) = match kind.as_str() {
        "idp-connections" => ("idp_connection", "idp_connection"),
        "scim-connections" => ("scim_connection", "scim_connection"),
        "entitlement-bindings" => ("entitlement_binding", "entitlement_binding"),
        _ => return Err(not_found("unsupported organization resource")),
    };
    reject_plaintext_secret_payload(&payload)?;
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            slug_id(
                resource_type,
                &org_id,
                payload
                    .get("label")
                    .or_else(|| payload.get("client_id"))
                    .or_else(|| payload.get("entitlement_id"))
                    .and_then(Value::as_str)
                    .unwrap_or(resource_type),
            )
        });
    let mut resource = payload;
    resource["id"] = json!(id);
    resource["account_id"] = json!(org.account_id);
    resource["organization_id"] = json!(org_id);
    let saved = catalog
        .upsert_control_plane_resource(
            resource_type,
            resource["id"].as_str().unwrap(),
            &org.account_id,
            Some(&org_id),
            None,
            &resource,
        )
        .map_err(server_error)?;
    let action = format!("control_plane.{resource_type}.create");
    audit_control_plane(
        &catalog,
        &actor,
        &org.account_id,
        Some(&org_id),
        None,
        &action,
        resource_type,
        &saved.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(json!({"status":"ok", response_key: saved.resource})))
}

async fn upsert_audit_export_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path(org_id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let org = organization_owner(&catalog, &org_id)?;
    if let Err(e) = ensure_account_actor(&actor, &org.account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &org.account_id,
            Some(&org_id),
            None,
            "control_plane.audit_export.configure",
            "audit_export",
            &format!("audit_export_{org_id}"),
        );
        return Err(e);
    }
    reject_plaintext_secret_payload(&payload)?;
    let mut resource = payload;
    resource["id"] = json!(format!("audit_export_{org_id}"));
    resource["account_id"] = json!(org.account_id);
    resource["organization_id"] = json!(org_id);
    let saved = catalog
        .upsert_control_plane_resource(
            "audit_export",
            resource["id"].as_str().unwrap(),
            &org.account_id,
            Some(&org_id),
            None,
            &resource,
        )
        .map_err(server_error)?;
    audit_control_plane(
        &catalog,
        &actor,
        &org.account_id,
        Some(&org_id),
        None,
        "control_plane.audit_export.configure",
        "audit_export",
        &saved.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(json!({"status":"ok", "audit_export": saved.resource})))
}

async fn get_audit_export_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path(org_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let org = organization_owner(&catalog, &org_id)?;
    if let Err(e) = ensure_account_actor(&actor, &org.account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &org.account_id,
            Some(&org_id),
            None,
            "control_plane.audit_export.read",
            "audit_export",
            &format!("audit_export_{org_id}"),
        );
        return Err(e);
    }
    let config = catalog
        .get_control_plane_resource("audit_export", &format!("audit_export_{org_id}"))
        .map_err(server_error)?
        .map(|r| r.resource);
    let events = catalog
        .search_audit_events(&org.account_id, Some(&org_id), 200)
        .map_err(server_error)?
        .into_iter()
        .map(|e| {
            let mut event = serde_json::to_value(e.event).unwrap_or_else(|_| json!({}));
            if let Some(decision) = event
                .get("metadata_redacted")
                .and_then(|m| m.get("decision"))
                .cloned()
            {
                event["decision"] = decision;
            }
            event
        })
        .collect::<Vec<_>>();
    Ok(Json(
        json!({"status":"ok", "audit_export": config, "events": events}),
    ))
}

async fn create_workspace_resource_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path((workspace_id, kind)): Path<(String, String)>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let workspace = workspace_owner(&catalog, &workspace_id)?;
    if let Err(e) = ensure_account_actor(&actor, &workspace.account_id) {
        audit_control_plane(
            &catalog,
            &actor,
            &workspace.account_id,
            workspace.organization_id.as_deref(),
            Some(&workspace_id),
            &format!(
                "control_plane.{}.create",
                kind.trim_end_matches('s').replace('-', "_")
            ),
            &kind,
            &workspace_id,
            AuditResult::Denied,
            json!({"decision":"denied", "reason":"cross_account_or_workspace"}),
        );
        return Err(e);
    }
    let (resource_type, response_key) = match kind.as_str() {
        "warehouse-bindings" => ("warehouse_binding", "warehouse_binding"),
        "object-storage-bindings" => ("object_storage_binding", "object_storage_binding"),
        "secret-handles" => ("secret_handle", "secret_handle"),
        _ => return Err(not_found("unsupported workspace resource")),
    };
    reject_plaintext_secret_payload(&payload)?;
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            slug_id(
                resource_type,
                &workspace_id,
                payload
                    .get("warehouse_name")
                    .or_else(|| payload.get("handle"))
                    .or_else(|| payload.get("bucket"))
                    .and_then(Value::as_str)
                    .unwrap_or(resource_type),
            )
        });
    let mut resource = payload;
    resource["id"] = json!(id);
    resource["account_id"] = json!(workspace.account_id);
    resource["organization_id"] = json!(workspace.organization_id);
    resource["workspace_id"] = json!(workspace_id);
    let saved = catalog
        .upsert_control_plane_resource(
            resource_type,
            resource["id"].as_str().unwrap(),
            &workspace.account_id,
            workspace.organization_id.as_deref(),
            Some(&workspace_id),
            &resource,
        )
        .map_err(server_error)?;
    let action = format!("control_plane.{resource_type}.create");
    audit_control_plane(
        &catalog,
        &actor,
        &workspace.account_id,
        workspace.organization_id.as_deref(),
        Some(&workspace_id),
        &action,
        resource_type,
        &saved.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(json!({"status":"ok", response_key: saved.resource})))
}

async fn create_workspace_service_client_control_plane(
    State(state): State<AuthState>,
    actor: AuthContext,
    Path(workspace_id): Path<String>,
    Json(mut req): Json<ServiceClientCreateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let catalog = require_catalog(&state)?;
    let workspace = workspace_owner(&catalog, &workspace_id)?;
    if let Err(e) = ensure_account_actor(&actor, &workspace.account_id) {
        audit_denied_control_plane(
            &catalog,
            &actor,
            &workspace.account_id,
            workspace.organization_id.as_deref(),
            Some(&workspace_id),
            "control_plane.service_client.create",
            "service_client",
            &workspace_id,
        );
        return Err(e);
    }
    req.account_id = workspace.account_id.clone();
    req.workspace_id = Some(workspace_id.clone());
    let client_secret = req.client_secret.unwrap_or_else(generated_client_secret);
    let record = catalog
        .create_service_identity_client(ServiceIdentityClientInput {
            id: req.client_id,
            account_id: req.account_id,
            workspace_id: req.workspace_id,
            secret_hash: ClientRegistry::secret_hash_for_secret(&client_secret),
            role: req.role,
            scopes: req.scopes,
            expires_at: req.expires_at,
        })
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"status":"error", "message": e.to_string()})),
            )
        })?;
    state
        .clients
        .register_durable_client(record.clone())
        .map_err(server_error)?;
    let snapshot = ServiceClientApiSnapshot::from(record.clone());
    let saved_resource = serde_json::to_value(&snapshot).map_err(server_error)?;
    catalog
        .upsert_control_plane_resource(
            "service_client",
            &record.id,
            &record.account_id,
            workspace.organization_id.as_deref(),
            record.workspace_id.as_deref(),
            &saved_resource,
        )
        .map_err(server_error)?;
    audit_control_plane(
        &catalog,
        &actor,
        &record.account_id,
        workspace.organization_id.as_deref(),
        Some(&workspace_id),
        "control_plane.service_client.create",
        "service_client",
        &record.id,
        AuditResult::Allowed,
        json!({"decision":"allowed"}),
    );
    Ok(Json(
        json!({"status":"ok", "client": snapshot, "client_secret": client_secret, "secret_delivery":"shown_once"}),
    ))
}

async fn list_service_clients(
    State(state): State<AuthState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(catalog) = state.catalog().map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": message })),
        )
    })?
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "error", "message": "durable service identity catalog is not configured" }),
            ),
        ));
    };
    let clients = catalog
        .list_service_identity_clients(None)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?
        .into_iter()
        .map(ServiceClientApiSnapshot::from)
        .collect::<Vec<_>>();
    Ok(Json(json!({ "status": "ok", "clients": clients })))
}

async fn create_service_client(
    State(state): State<AuthState>,
    Json(req): Json<ServiceClientCreateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(catalog) = state.catalog().map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": message })),
        )
    })?
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "error", "message": "durable service identity catalog is not configured" }),
            ),
        ));
    };
    let client_secret = req.client_secret.unwrap_or_else(generated_client_secret);
    let record = catalog
        .create_service_identity_client(ServiceIdentityClientInput {
            id: req.client_id,
            account_id: req.account_id,
            workspace_id: req.workspace_id,
            secret_hash: ClientRegistry::secret_hash_for_secret(&client_secret),
            role: req.role,
            scopes: req.scopes,
            expires_at: req.expires_at,
        })
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?;
    state
        .clients
        .register_durable_client(record.clone())
        .map_err(|message| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": message })),
            )
        })?;
    Ok(Json(json!({
        "status": "ok",
        "client": ServiceClientApiSnapshot::from(record),
        "client_secret": client_secret,
        "secret_delivery": "shown_once"
    })))
}

async fn rotate_service_client(
    State(state): State<AuthState>,
    Path(client_id): Path<String>,
    Json(req): Json<ServiceClientRotateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(catalog) = state.catalog().map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": message })),
        )
    })?
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "error", "message": "durable service identity catalog is not configured" }),
            ),
        ));
    };
    let client_secret = req.client_secret.unwrap_or_else(generated_client_secret);
    catalog
        .rotate_service_identity_secret(
            &client_id,
            &ClientRegistry::secret_hash_for_secret(&client_secret),
        )
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?;
    let record = catalog
        .get_service_identity_client(&client_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": "service client not found" })),
            )
        })?;
    state
        .clients
        .register_durable_client(record.clone())
        .map_err(|message| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": message })),
            )
        })?;
    Ok(Json(json!({
        "status": "ok",
        "client": ServiceClientApiSnapshot::from(record),
        "client_secret": client_secret,
        "secret_delivery": "shown_once"
    })))
}

async fn revoke_service_client(
    State(state): State<AuthState>,
    Path(client_id): Path<String>,
    Json(req): Json<StatusChangeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(catalog) = state.catalog().map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": message })),
        )
    })?
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                json!({ "status": "error", "message": "durable service identity catalog is not configured" }),
            ),
        ));
    };
    let reason = req
        .reason
        .unwrap_or_else(|| "operator revoked service client".to_string());
    catalog
        .revoke_service_identity_client(&client_id, &reason)
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?;
    if let Some(record) = catalog
        .get_service_identity_client(&client_id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "message": e.to_string() })),
            )
        })?
    {
        state
            .clients
            .register_durable_client(record)
            .map_err(|message| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "status": "error", "message": message })),
                )
            })?;
    }
    Ok(Json(
        json!({ "status": "ok", "client_id": client_id, "account_status": "revoked" }),
    ))
}

#[derive(Debug, Deserialize)]
struct EvaluationRegisterRequest {
    #[serde(default)]
    email: Option<String>,
}

async fn register_evaluation_account(
    State(state): State<AuthState>,
    Json(req): Json<EvaluationRegisterRequest>,
) -> Json<Value> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let client_id = format!("eval-{nonce:x}");
    let tenant_id = format!("eval-tenant-{nonce:x}");
    let client_secret = format!("opensnow-eval-{nonce:x}-sandbox-secret");
    let quota = std::env::var("OPENSNOW_EVALUATION_QUERY_QUOTA")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|quota| *quota > 0)
        .unwrap_or(DEFAULT_EVALUATION_QUERY_QUOTA);

    state
        .clients
        .register_evaluation_client(&client_id, &client_secret, &tenant_id, quota);

    Json(json!({
        "status": "ok",
        "mode": "evaluation_sandbox",
        "client_id": client_id,
        "client_secret": client_secret,
        "tenant_id": tenant_id,
        "query_quota": quota,
        "token_endpoint": "/auth/token",
        "sample_dataset": "opensnow_demo_orders",
        "requested_email": req.email,
        "guardrails": ["single-statement SQL", "destructive SQL blocked", "evaluation tenant isolation"],
        "upgrade_path": "When evaluation is complete, create an enterprise BYOC account and migrate workloads to customer-owned infrastructure; sandbox credentials and demo data do not carry production authority.",
    }))
}

#[derive(Debug, Deserialize)]
struct StatusChangeRequest {
    #[serde(default)]
    reason: Option<String>,
}

async fn list_evaluation_accounts(State(state): State<AuthState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "accounts": state.clients.evaluation_snapshots(),
        "mode": "evaluation_sandbox",
        "production_boundary": "evaluation accounts are isolated from enterprise BYOC accounts",
    }))
}

async fn suspend_evaluation_account(
    State(state): State<AuthState>,
    Path(client_id): Path<String>,
    Json(req): Json<StatusChangeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let reason = req
        .reason
        .unwrap_or_else(|| "operator suspended evaluation account".to_string());
    state
        .clients
        .suspend_client(&client_id, &reason)
        .map_err(|message| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": message })),
            )
        })?;
    Ok(Json(
        json!({ "status": "ok", "client_id": client_id, "account_status": "suspended" }),
    ))
}

async fn revoke_evaluation_account(
    State(state): State<AuthState>,
    Path(client_id): Path<String>,
    Json(req): Json<StatusChangeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let reason = req
        .reason
        .unwrap_or_else(|| "operator revoked evaluation account".to_string());
    state
        .clients
        .revoke_client(&client_id, &reason)
        .map_err(|message| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": message })),
            )
        })?;
    Ok(Json(
        json!({ "status": "ok", "client_id": client_id, "account_status": "revoked" }),
    ))
}

#[derive(Debug, Deserialize, Default)]
struct ScimListQuery {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default, rename = "startIndex")]
    start_index: Option<usize>,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Debug, Deserialize, Default)]
struct ScimTokenRotateRequest {
    #[serde(default)]
    label: Option<String>,
}

fn bearer_secret(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn scim_account_from_headers(
    state: &AuthState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let token = bearer_secret(headers)
        .and_then(|secret| state.scim.authenticate_token(secret))
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "schemas": ["urn:ietf:params:scim:api:messages:2.0:Error"], "status": "401", "detail": "invalid SCIM bearer token" })),
            )
        })?;
    if let Some(requested) = headers
        .get("X-Account-ID")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.trim().is_empty())
    {
        if requested != token.account_id {
            return Err((
                StatusCode::FORBIDDEN,
                Json(
                    json!({ "schemas": ["urn:ietf:params:scim:api:messages:2.0:Error"], "status": "403", "detail": "SCIM token is scoped to a different account" }),
                ),
            ));
        }
    }
    Ok(token.account_id)
}

fn scim_user_json(user: &ScimUserRecord) -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "id": user.id,
        "externalId": user.external_id,
        "userName": user.user_name,
        "displayName": user.display_name,
        "active": user.active,
        "meta": {
            "resourceType": "User",
            "created": user.created_at,
            "lastModified": user.updated_at,
        }
    })
}

fn scim_group_json(group: &ScimGroupRecord) -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:Group"],
        "id": group.id,
        "displayName": group.display_name,
        "members": group.members.iter().map(|id| json!({ "value": id })).collect::<Vec<_>>(),
        "urn:opensnow:scim:role": group.role,
        "meta": {
            "resourceType": "Group",
            "created": group.created_at,
            "lastModified": group.updated_at,
            "tombstoned": group.tombstoned,
        }
    })
}

fn filter_user_name(filter: Option<&str>) -> Option<String> {
    let raw = filter?.trim();
    let lower = raw.to_ascii_lowercase();
    let prefix = "username eq ";
    if !lower.starts_with(prefix) {
        return None;
    }
    let value = raw[prefix.len()..]
        .trim()
        .trim_matches('"')
        .to_ascii_lowercase();
    (!value.is_empty()).then_some(value)
}

async fn scim_response_content_type(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    if response.status() != StatusCode::NO_CONTENT {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/scim+json"),
        );
    }
    response
}

fn authorize_scim_token_account(
    auth: &AuthContext,
    account_id: &str,
) -> Result<bool, (StatusCode, Json<Value>)> {
    let break_glass = auth.has_explicit_scope("break_glass.admin");
    if auth.tenant_id == account_id || break_glass {
        Ok(break_glass)
    } else {
        warn!(
            actor = %auth.username,
            actor_tenant = %auth.tenant_id,
            requested_account = account_id,
            "rejected cross-account SCIM token administration attempt"
        );
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "status": "error",
                "message": "SCIM token administration is scoped to the authenticated account"
            })),
        ))
    }
}

fn scim_token_snapshot_json(token: &ScimTokenSnapshot) -> Value {
    json!({
        "id": token.id,
        "account_id": token.account_id,
        "label": token.label,
        "created_at": token.created_at,
        "revoked_at": token.revoked_at,
    })
}

async fn scim_rotate_token(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    auth: AuthContext,
    Json(req): Json<ScimTokenRotateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let break_glass = authorize_scim_token_account(&auth, &account_id)?;
    let issued = state.scim.rotate_token(
        &account_id,
        req.label.as_deref().unwrap_or("scim"),
        &auth.username,
        break_glass,
    );
    Ok(Json(json!({
        "status": "ok",
        "token": {
            "id": issued.id,
            "account_id": issued.account_id,
            "label": issued.label,
            "secret": issued.secret,
            "secret_hash": Value::Null,
            "created_at": issued.created_at,
        }
    })))
}

async fn scim_list_tokens(
    State(state): State<AuthState>,
    Path(account_id): Path<String>,
    auth: AuthContext,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _break_glass = authorize_scim_token_account(&auth, &account_id)?;
    let tokens = state.scim.list_tokens(&account_id);
    Ok(Json(json!({
        "status": "ok",
        "account_id": account_id,
        "tokens": tokens.iter().map(scim_token_snapshot_json).collect::<Vec<_>>()
    })))
}

async fn scim_revoke_token(
    State(state): State<AuthState>,
    Path((account_id, token_id)): Path<(String, String)>,
    auth: AuthContext,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let break_glass = authorize_scim_token_account(&auth, &account_id)?;
    state
        .scim
        .revoke_token(&account_id, &token_id, &auth.username, break_glass)
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "error", "message": e })),
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

async fn scim_create_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let user = state.scim.upsert_user(&account_id, None, &payload);
    Ok((StatusCode::CREATED, Json(scim_user_json(&user))))
}

async fn scim_list_users(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<ScimListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let filter = filter_user_name(query.filter.as_deref());
    let mut users = state.scim.list_users(&account_id, filter.as_deref());
    let total = users.len();
    let start = query.start_index.unwrap_or(1).saturating_sub(1);
    let count = query.count.unwrap_or(100).min(200);
    users = users.into_iter().skip(start).take(count).collect();
    Ok(Json(json!({
        "schemas": ["urn:ietf:params:scim:api:messages:2.0:ListResponse"],
        "totalResults": total,
        "startIndex": start + 1,
        "itemsPerPage": users.len(),
        "Resources": users.iter().map(scim_user_json).collect::<Vec<_>>()
    })))
}

async fn scim_get_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let user = state
        .scim
        .user_snapshot(&account_id, &user_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "404", "detail": "unknown SCIM user" })),
            )
        })?;
    Ok(Json(scim_user_json(&user)))
}

async fn scim_put_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let user = state
        .scim
        .upsert_user(&account_id, Some(&user_id), &payload);
    if !user.active {
        let _ = state
            .clients
            .revoke_client(&user.user_name, "SCIM user deactivated");
    }
    Ok(Json(scim_user_json(&user)))
}

async fn scim_patch_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let mut active = None;
    if let Some(ops) = payload.get("Operations").and_then(Value::as_array) {
        for op in ops {
            if op
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| p.eq_ignore_ascii_case("active"))
            {
                active = op.get("value").and_then(Value::as_bool);
            }
        }
    }
    let user = if let Some(active) = active {
        state
            .scim
            .set_user_active(&account_id, &user_id, active)
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "status": "404", "detail": "unknown SCIM user" })),
                )
            })?
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "400", "detail": "unsupported SCIM PATCH operation" })),
        ));
    };
    if !user.active {
        let _ = state
            .clients
            .revoke_client(&user.user_name, "SCIM user deactivated");
    }
    Ok(Json(scim_user_json(&user)))
}

async fn scim_delete_user(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let user = state
        .scim
        .set_user_active(&account_id, &user_id, false)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "404", "detail": "unknown SCIM user" })),
            )
        })?;
    let _ = state
        .clients
        .revoke_client(&user.user_name, "SCIM user deactivated");
    Ok(StatusCode::NO_CONTENT)
}

async fn scim_create_group(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let group = state.scim.upsert_group(&account_id, None, &payload);
    Ok((StatusCode::CREATED, Json(scim_group_json(&group))))
}

async fn scim_list_groups(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(query): Query<ScimListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let mut groups = state.scim.list_groups(&account_id);
    let total = groups.len();
    let start = query.start_index.unwrap_or(1).saturating_sub(1);
    let count = query.count.unwrap_or(100).min(200);
    groups = groups.into_iter().skip(start).take(count).collect();
    Ok(Json(json!({
        "schemas": ["urn:ietf:params:scim:api:messages:2.0:ListResponse"],
        "totalResults": total,
        "startIndex": start + 1,
        "itemsPerPage": groups.len(),
        "Resources": groups.iter().map(scim_group_json).collect::<Vec<_>>()
    })))
}

async fn scim_get_group(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let group = state
        .scim
        .group_snapshot(&account_id, &group_id)
        .filter(|g| !g.tombstoned)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "404", "detail": "unknown SCIM group" })),
            )
        })?;
    Ok(Json(scim_group_json(&group)))
}

async fn scim_put_group(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    let group = state
        .scim
        .upsert_group(&account_id, Some(&group_id), &payload);
    Ok(Json(scim_group_json(&group)))
}

async fn scim_delete_group(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<Value>)> {
    let account_id = scim_account_from_headers(&state, &headers)?;
    state
        .scim
        .tombstone_group(&account_id, &group_id)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "status": "404", "detail": "unknown SCIM group" })),
            )
        })?;
    Ok(StatusCode::NO_CONTENT)
}

/// Build a router exposing `POST /auth/token` plus admin-only evaluation
/// account controls.
pub fn auth_router(state: AuthState) -> Router {
    let protected = Router::new()
        .route("/api/v1/evaluation/accounts", get(list_evaluation_accounts))
        .route(
            "/api/v1/admin/accounts/{account_id}/scim/tokens",
            get(scim_list_tokens).post(scim_rotate_token),
        )
        .route(
            "/api/v1/admin/accounts/{account_id}/scim/tokens/{token_id}",
            delete(scim_revoke_token),
        )
        .route(
            "/api/v1/service-clients",
            get(list_service_clients).post(create_service_client),
        )
        .route(
            "/api/v1/service-clients/{client_id}/rotate",
            post(rotate_service_client),
        )
        .route(
            "/api/v1/service-clients/{client_id}/revoke",
            post(revoke_service_client),
        )
        .route("/api/v1/accounts", post(create_account_control_plane))
        .route(
            "/api/v1/accounts/{account_id}/organizations",
            post(create_organization_control_plane),
        )
        .route(
            "/api/v1/organizations/{org_id}/workspaces",
            post(create_workspace_control_plane),
        )
        .route(
            "/api/v1/organizations/{org_id}/audit/export",
            get(get_audit_export_control_plane).post(upsert_audit_export_control_plane),
        )
        .route(
            "/api/v1/organizations/{org_id}/{kind}",
            post(create_org_resource_control_plane),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/service-clients",
            post(create_workspace_service_client_control_plane),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/{kind}",
            post(create_workspace_resource_control_plane),
        )
        .route(
            "/api/v1/evaluation/accounts/{client_id}/suspend",
            post(suspend_evaluation_account),
        )
        .route(
            "/api/v1/evaluation/accounts/{client_id}/revoke",
            post(revoke_evaluation_account),
        )
        .route_layer(axum::middleware::from_fn(require_admin_scope))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            jwt_required,
        ))
        .with_state(state.clone());

    let scim = Router::new()
        .route(
            "/scim/v2/Users",
            get(scim_list_users).post(scim_create_user),
        )
        .route(
            "/scim/v2/Users/{user_id}",
            get(scim_get_user)
                .put(scim_put_user)
                .patch(scim_patch_user)
                .delete(scim_delete_user),
        )
        .route(
            "/scim/v2/Groups",
            get(scim_list_groups).post(scim_create_group),
        )
        .route(
            "/scim/v2/Groups/{group_id}",
            get(scim_get_group)
                .put(scim_put_group)
                .delete(scim_delete_group),
        )
        .route_layer(middleware::from_fn(scim_response_content_type))
        .with_state(state.clone());

    Router::new()
        .route("/auth/token", post(token_endpoint))
        .route("/auth/jwks.json", get(jwks_endpoint))
        .route("/.well-known/jwks.json", get(jwks_endpoint))
        .route(
            "/api/v1/evaluation/register",
            post(register_evaluation_account),
        )
        .with_state(state)
        .merge(scim)
        .merge(protected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request as HttpRequest;
    use opensnow_auth::{JwtManager, SsoSessionTokenRequest};
    use tower::ServiceExt;

    async fn protected_ok() -> &'static str {
        "ok"
    }

    fn protected_app(state: AuthState) -> Router {
        Router::new()
            .route("/protected", get(protected_ok))
            .route_layer(axum::middleware::from_fn_with_state(state, jwt_required))
    }

    fn auth_state() -> AuthState {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register("svc-a", "secret-a", "ANALYST");
        AuthState::new(jwt, clients, 1)
    }

    #[tokio::test]
    async fn token_endpoint_issues_jwt_for_valid_client() {
        let state = auth_state();
        let app = auth_router(state.clone());

        let body = serde_json::to_string(&json!({
            "grant_type": "client_credentials",
            "client_id": "svc-a",
            "client_secret": "secret-a",
        }))
        .unwrap();

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let parsed: TokenResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.token_type, "Bearer");
        assert_eq!(parsed.scope, "ANALYST");
        // Validate the JWT round-trips with the manager.
        let claims = state.jwt.validate_token(&parsed.access_token).unwrap();
        assert_eq!(claims.username, "svc-a");
        assert_eq!(claims.role, "ANALYST");
    }

    #[tokio::test]
    async fn token_endpoint_preserves_registered_tenant_and_scopes() {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_with_metadata(
            "svc-ci",
            "secret-ci",
            "ANALYST",
            "org-acme",
            vec!["sql.query".to_string(), "table.select".to_string()],
        );
        let state = AuthState::new(jwt, clients, 1);
        let app = auth_router(state.clone());

        let body = serde_json::to_string(&json!({
            "grant_type": "client_credentials",
            "client_id": "svc-ci",
            "client_secret": "secret-ci",
        }))
        .unwrap();

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let parsed: TokenResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.scope, "sql.query table.select");
        let claims = state.jwt.validate_token(&parsed.access_token).unwrap();
        assert_eq!(claims.tenant_id, "org-acme");
        assert_eq!(claims.scopes, vec!["sql.query", "table.select"]);
    }

    #[tokio::test]
    async fn jwt_required_rechecks_durable_oidc_session_and_fails_closed_after_revocation() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let sso = opensnow_auth::SsoManager::open(temp.path()).unwrap();
        sso.upsert_idp_connection(opensnow_auth::IdpConnectionUpsert {
            account_id: "acme-corp".to_string(),
            connection_id: "okta".to_string(),
            protocol: opensnow_auth::SsoProtocol::Oidc,
            enabled: true,
            issuer: "https://idp.acme.example".to_string(),
            client_id: "opensnow".to_string(),
            client_secret: None,
            client_secret_handle: None,
            allowed_domains: vec!["acme.example".to_string()],
            scopes: None,
        })
        .unwrap();
        let session_id = sso
            .create_sso_session(
                "acme-corp",
                "okta",
                "oidc-sub-123",
                "alice@acme.example",
                &["sql.query".to_string(), "table.select".to_string()],
                3600,
            )
            .unwrap();

        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        let state = AuthState::new(jwt, clients, 1)
            .with_sso_session_store(temp.path().to_string_lossy().to_string());
        let token = state
            .jwt
            .generate_sso_session_token(SsoSessionTokenRequest {
                user_id: 42,
                username: "alice@acme.example",
                role: "ANALYST",
                tenant_id: "acme-corp",
                scopes: vec!["sql.query".to_string(), "table.select".to_string()],
                session_id: &session_id,
                expiry_hours: 1,
            })
            .unwrap();
        let app = protected_app(state.clone());

        let ok = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);

        sso.revoke_sso_session(&session_id).unwrap();
        let denied = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    }

    fn bearer_for_account(
        state: &AuthState,
        client_id: &str,
        role: &str,
        account_id: &str,
        scopes: Vec<&str>,
    ) -> String {
        state
            .jwt
            .generate_token_with_scopes(
                0,
                client_id,
                role,
                account_id,
                scopes.into_iter().map(ToOwned::to_owned).collect(),
                1,
            )
            .unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn scim_token_admin_routes_reject_cross_account_mutation_before_state_changes() {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_with_metadata(
            "acme-admin",
            "secret-acme",
            "ACCOUNTADMIN",
            "acme-corp",
            vec!["policy.admin".to_string()],
        );
        clients.register_with_metadata(
            "beta-admin",
            "secret-beta",
            "ACCOUNTADMIN",
            "beta-inc",
            vec!["policy.admin".to_string()],
        );
        clients.register_with_metadata(
            "platform-wildcard",
            "secret-wildcard",
            "SYSADMIN",
            "platform-root",
            vec!["*".to_string()],
        );
        clients.register_with_metadata(
            "platform-break-glass",
            "secret-root",
            "SYSADMIN",
            "platform-root",
            vec!["policy.admin".to_string(), "break_glass.admin".to_string()],
        );
        let state = AuthState::new(jwt, clients, 1);
        let acme_admin = bearer_for_account(
            &state,
            "acme-admin",
            "ACCOUNTADMIN",
            "acme-corp",
            vec!["policy.admin"],
        );
        let beta_admin = bearer_for_account(
            &state,
            "beta-admin",
            "ACCOUNTADMIN",
            "beta-inc",
            vec!["policy.admin"],
        );
        let wildcard_admin = bearer_for_account(
            &state,
            "platform-wildcard",
            "SYSADMIN",
            "platform-root",
            vec!["*"],
        );
        let break_glass = bearer_for_account(
            &state,
            "platform-break-glass",
            "SYSADMIN",
            "platform-root",
            vec!["policy.admin", "break_glass.admin"],
        );
        let app = auth_router(state.clone());

        let beta_created = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/admin/accounts/beta-inc/scim/tokens")
                    .header("authorization", format!("Bearer {beta_admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "label": "beta-okta" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(beta_created.status(), StatusCode::OK);
        let beta_body = response_json(beta_created).await;
        let beta_token_id = beta_body["token"]["id"].as_str().unwrap();
        let beta_secret = beta_body["token"]["secret"].as_str().unwrap();

        for (method, uri, body, bearer, actor) in [
            (
                "POST",
                "/api/v1/admin/accounts/beta-inc/scim/tokens",
                json!({ "label": "forged-beta" }).to_string(),
                &acme_admin,
                "acme-admin",
            ),
            (
                "GET",
                "/api/v1/admin/accounts/beta-inc/scim/tokens",
                String::new(),
                &acme_admin,
                "acme-admin",
            ),
            (
                "DELETE",
                &format!("/api/v1/admin/accounts/beta-inc/scim/tokens/{beta_token_id}"),
                String::new(),
                &acme_admin,
                "acme-admin",
            ),
            (
                "POST",
                "/api/v1/admin/accounts/beta-inc/scim/tokens",
                json!({ "label": "wildcard-is-not-break-glass" }).to_string(),
                &wildcard_admin,
                "platform-wildcard",
            ),
        ] {
            let denied = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method(method)
                        .uri(uri)
                        .header("authorization", format!("Bearer {bearer}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                denied.status(),
                StatusCode::FORBIDDEN,
                "{method} {uri} {actor}"
            );
        }

        assert!(state.scim.authenticate_token(beta_secret).is_some());
        let audit = state.scim.audit_events();
        assert!(!audit.iter().any(|event| {
            event["account_id"] == "beta-inc"
                && event["actor"] == "acme-admin"
                && matches!(
                    event["action"].as_str(),
                    Some("scim.token.rotate" | "scim.token.revoke")
                )
        }));

        let listed = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("GET")
                    .uri("/api/v1/admin/accounts/beta-inc/scim/tokens")
                    .header("authorization", format!("Bearer {break_glass}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let listed = response_json(listed).await;
        assert_eq!(listed["tokens"].as_array().unwrap().len(), 1);
        assert!(listed["tokens"][0].get("secret_hash").is_none());

        let break_glass_created = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/admin/accounts/beta-inc/scim/tokens")
                    .header("authorization", format!("Bearer {break_glass}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "label": "emergency" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(break_glass_created.status(), StatusCode::OK);
        let audit = state.scim.audit_events();
        assert!(audit.iter().any(|event| {
            event["action"] == "scim.token.rotate"
                && event["account_id"] == "beta-inc"
                && event["actor"] == "platform-break-glass"
                && event["break_glass"] == true
        }));
    }

    #[test]
    fn durable_scim_directory_reopens_state_and_exports_shared_audit_events() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let path = catalog_path.to_string_lossy().to_string();
        let scim = ScimDirectory::durable(path.clone());
        let issued = scim.rotate_token("acme", "okta", "admin", false);
        assert!(scim.authenticate_token(&issued.secret).is_some());
        let user = scim.upsert_user(
            "acme",
            None,
            &json!({ "userName": "ada@example.com", "displayName": "Ada" }),
        );
        let group = scim.upsert_group(
            "acme",
            None,
            &json!({ "displayName": "ACCOUNTADMIN", "members": [{ "value": user.id }] }),
        );
        scim.set_user_active("acme", &user.id, false).unwrap();
        scim.tombstone_group("acme", &group.id).unwrap();
        drop(scim);

        let reopened = ScimDirectory::durable(path.clone());
        assert!(reopened.authenticate_token(&issued.secret).is_some());
        assert_eq!(reopened.list_tokens("acme").len(), 1);

        let rotated_after_reopen = reopened.rotate_token("acme", "okta-rotated", "admin", false);
        assert_ne!(issued.id, rotated_after_reopen.id);
        let tokens = reopened.list_tokens("acme");
        assert_eq!(tokens.len(), 2);
        assert!(tokens.iter().any(|token| token.id == issued.id));
        assert!(
            tokens
                .iter()
                .any(|token| token.id == rotated_after_reopen.id)
        );
        assert!(reopened.authenticate_token(&issued.secret).is_some());
        assert!(
            reopened
                .authenticate_token(&rotated_after_reopen.secret)
                .is_some()
        );
        let catalog_tokens = Catalog::open(&path).unwrap().all_scim_tokens().unwrap();
        assert_eq!(catalog_tokens.len(), 2);
        assert!(
            catalog_tokens
                .iter()
                .all(|token| !token.secret_hash.contains("scim_acme"))
        );

        assert_eq!(
            reopened.user_snapshot("acme", &user.id).unwrap().lifecycle,
            "deactivated"
        );
        assert!(
            reopened
                .group_snapshot("acme", &group.id)
                .unwrap()
                .tombstoned
        );

        let catalog = Catalog::open(&path).unwrap();
        assert!(
            !catalog.all_scim_tokens().unwrap()[0]
                .secret_hash
                .contains(&issued.secret)
        );
        let audit = catalog.search_audit_events("acme", None, 20).unwrap();
        assert!(
            audit
                .iter()
                .any(|event| event.action == "scim.token.rotate")
        );
        assert!(
            audit
                .iter()
                .any(|event| event.action == "scim.user.deactivate")
        );
        assert!(
            audit
                .iter()
                .any(|event| event.action == "scim.group.tombstone")
        );
    }

    #[tokio::test]
    async fn durable_service_clients_create_list_rotate_revoke_and_gate_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let catalog = Catalog::open(catalog_path.to_str().unwrap()).unwrap();
        catalog
            .register_account("Acme Corp", "owner@acme.test")
            .unwrap();

        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_with_metadata(
            "admin-client",
            "admin-secret",
            "ACCOUNTADMIN",
            "acme-corp-default",
            vec!["policy.admin".to_string()],
        );
        let state = AuthState::new(jwt, clients, 1)
            .with_durable_service_catalog_path(catalog_path.to_string_lossy().to_string());
        let admin = state
            .jwt
            .generate_token_with_scopes(
                0,
                "admin-client",
                "ACCOUNTADMIN",
                "acme-corp-default",
                vec!["policy.admin".to_string()],
                1,
            )
            .unwrap();
        let app = auth_router(state.clone());

        let create_body = json!({
            "client_id": "svc-acme-loader",
            "account_id": "acme-corp",
            "workspace_id": "acme-corp-default",
            "role": "ANALYST",
            "scopes": ["sql.query", "table.select"],
            "client_secret": "secret-v1"
        });
        let created = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/service-clients")
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(create_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::OK);
        let created_body: Value =
            serde_json::from_slice(&to_bytes(created.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(created_body["client_secret"], "secret-v1");
        assert_eq!(created_body["secret_delivery"], "shown_once");
        assert!(created_body["client"].get("secret_hash").is_none());

        let listed = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/v1/service-clients")
                    .header("authorization", format!("Bearer {admin}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(listed.status(), StatusCode::OK);
        let list_body: Value =
            serde_json::from_slice(&to_bytes(listed.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(list_body["clients"].as_array().unwrap().len(), 1);
        assert!(list_body["clients"][0].get("secret_hash").is_none());

        let wrong_secret = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "grant_type": "client_credentials",
                            "client_id": "svc-acme-loader",
                            "client_secret": "wrong"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong_secret.status(), StatusCode::UNAUTHORIZED);

        let token_resp = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "grant_type": "client_credentials",
                            "client_id": "svc-acme-loader",
                            "client_secret": "secret-v1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token_resp.status(), StatusCode::OK);
        let token_body: TokenResponse =
            serde_json::from_slice(&to_bytes(token_resp.into_body(), 64 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(token_body.scope, "sql.query table.select");
        assert!(
            Catalog::open(catalog_path.to_str().unwrap())
                .unwrap()
                .get_service_identity_client("svc-acme-loader")
                .unwrap()
                .unwrap()
                .last_used_at
                .is_some()
        );

        let rotated = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/service-clients/svc-acme-loader/rotate")
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"client_secret":"secret-v2"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rotated.status(), StatusCode::OK);
        assert!(
            state
                .clients
                .authenticate_client("svc-acme-loader", "secret-v1")
                .is_none()
        );
        assert!(
            state
                .clients
                .authenticate_client("svc-acme-loader", "secret-v2")
                .is_some()
        );

        let revoked = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/service-clients/svc-acme-loader/revoke")
                    .header("authorization", format!("Bearer {admin}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"reason":"operator revoked"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);

        let protected = protected_app(state);
        let denied = protected
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header(
                        "authorization",
                        format!("Bearer {}", token_body.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn expired_durable_service_client_cannot_use_already_issued_token() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let catalog = Catalog::open(catalog_path.to_str().unwrap()).unwrap();
        catalog
            .register_account("Acme Corp", "owner@acme.test")
            .unwrap();

        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        let record = catalog
            .create_service_identity_client(ServiceIdentityClientInput {
                id: "svc-acme-expiring".to_string(),
                account_id: "acme-corp".to_string(),
                workspace_id: Some("acme-corp-default".to_string()),
                secret_hash: ClientRegistry::secret_hash_for_secret("secret-v1"),
                role: "ANALYST".to_string(),
                scopes: vec!["sql.query".to_string(), "table.select".to_string()],
                expires_at: Some("2099-01-01T00:00:00Z".to_string()),
            })
            .unwrap();
        clients.register_durable_client(record).unwrap();
        let state = AuthState::new(jwt, clients, 1)
            .with_durable_service_catalog_path(catalog_path.to_string_lossy().to_string());
        let app = auth_router(state.clone());

        let token_resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "grant_type": "client_credentials",
                            "client_id": "svc-acme-expiring",
                            "client_secret": "secret-v1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(token_resp.status(), StatusCode::OK);
        let token_body: TokenResponse =
            serde_json::from_slice(&to_bytes(token_resp.into_body(), 64 * 1024).await.unwrap())
                .unwrap();

        rusqlite::Connection::open(&catalog_path)
            .unwrap()
            .execute(
                "UPDATE service_identities SET expires_at = ?2 WHERE id = ?1",
                rusqlite::params!["svc-acme-expiring", "2000-01-01T00:00:00Z"],
            )
            .unwrap();

        let handler_hits = Arc::new(AtomicUsize::new(0));
        let hits_for_handler = handler_hits.clone();
        let protected = Router::new()
            .route(
                "/protected",
                get(move || {
                    let hits = hits_for_handler.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        "ok"
                    }
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(state, jwt_required));
        let denied = protected
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header(
                        "authorization",
                        format!("Bearer {}", token_body.access_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert_eq!(handler_hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn suspended_evaluation_client_cannot_use_already_issued_token() {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_evaluation_client("eval-alice", "secret-alice", "eval-alice-tenant", 10);
        let token = jwt
            .generate_token_with_scopes(
                0,
                "eval-alice",
                "EVALUATION",
                "eval-alice-tenant",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let state = AuthState::new(jwt, clients.clone(), 1);
        clients
            .suspend_client("eval-alice", "operator review")
            .unwrap();

        let resp = protected_app(state)
            .oneshot(
                HttpRequest::builder()
                    .uri("/protected")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn evaluation_query_quota_is_enforced_against_bearer_tokens() {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_evaluation_client("eval-bob", "secret-bob", "eval-bob-tenant", 1);
        let token = jwt
            .generate_token_with_scopes(
                0,
                "eval-bob",
                "EVALUATION",
                "eval-bob-tenant",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let state = AuthState::new(jwt, clients.clone(), 1);

        for expected_status in [StatusCode::OK, StatusCode::TOO_MANY_REQUESTS] {
            let resp = protected_app(state.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/protected")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), expected_status);
        }

        let snapshot = clients.describe_client_for_test("eval-bob").unwrap();
        assert_eq!(snapshot.queries_used, 1);
    }

    #[tokio::test]
    async fn evaluation_registration_returns_isolated_sandbox_credentials() {
        let state = AuthState::new(
            Arc::new(JwtManager::new(b"test-secret")),
            ClientRegistry::new(),
            1,
        );
        let app = auth_router(state.clone());

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/evaluation/register")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"email":"tester@example.com"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let client_id = body["client_id"].as_str().unwrap();
        let client_secret = body["client_secret"].as_str().unwrap();
        let tenant_id = body["tenant_id"].as_str().unwrap();

        assert!(client_id.starts_with("eval-"));
        assert!(client_secret.len() >= 32);
        assert!(tenant_id.starts_with("eval-"));
        assert_eq!(body["mode"], "evaluation_sandbox");
        assert!(
            body["upgrade_path"]
                .as_str()
                .unwrap()
                .contains("enterprise BYOC")
        );

        let stored = state.clients.describe_client_for_test(client_id).unwrap();
        assert_eq!(stored.kind, ClientAccountKind::Evaluation);
        assert_eq!(stored.tenant_id, tenant_id);
        assert_eq!(stored.query_quota, Some(DEFAULT_EVALUATION_QUERY_QUOTA));
        assert!(
            state
                .clients
                .authenticate_client(client_id, client_secret)
                .is_some()
        );
    }

    #[tokio::test]
    async fn token_endpoint_rejects_unknown_client() {
        let state = auth_state();
        let app = auth_router(state);

        let body = serde_json::to_string(&json!({
            "grant_type": "client_credentials",
            "client_id": "ghost",
            "client_secret": "x",
        }))
        .unwrap();

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_endpoint_rejects_wrong_secret() {
        let state = auth_state();
        let app = auth_router(state);

        let body = serde_json::to_string(&json!({
            "grant_type": "client_credentials",
            "client_id": "svc-a",
            "client_secret": "wrong",
        }))
        .unwrap();

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_endpoint_rejects_unsupported_grant() {
        let state = auth_state();
        let app = auth_router(state);

        let body = serde_json::to_string(&json!({
            "grant_type": "password",
            "client_id": "svc-a",
            "client_secret": "secret-a",
        }))
        .unwrap();

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/auth/token")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn parse_client_entry_accepts_enterprise_metadata() {
        let parsed = parse_client_entry("svc:secret:ANALYST:org-acme:sql.query table.select")
            .expect("valid enterprise client entry");

        assert_eq!(parsed.client_id, "svc");
        assert_eq!(parsed.secret, "secret");
        assert_eq!(parsed.role, "ANALYST");
        assert_eq!(parsed.tenant_id, "org-acme");
        assert_eq!(parsed.scopes, vec!["sql.query", "table.select"]);
    }

    #[test]
    fn registry_authenticate_round_trip() {
        let r = ClientRegistry::new();
        r.register("a", "s", "ADMIN");
        assert_eq!(r.authenticate("a", "s"), Some("ADMIN".to_string()));
        assert_eq!(r.authenticate("a", "wrong"), None);
        assert_eq!(r.authenticate("b", "s"), None);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn registry_does_not_store_raw_client_secret() {
        let r = ClientRegistry::new();
        r.register("svc", "top-secret-value", "ANALYST");

        let stored = r.describe_client_for_test("svc").unwrap();
        assert_ne!(stored.secret_hash, "top-secret-value");
        assert!(stored.secret_hash.starts_with("$argon2id$"));
        assert_eq!(
            r.authenticate("svc", "top-secret-value"),
            Some("ANALYST".to_string())
        );
    }

    #[tokio::test]
    async fn jwt_required_attaches_claims_and_rejects_tenant_spoofing() {
        let state = auth_state();
        let token = state
            .jwt
            .generate_token_for_tenant(7, "alice", "ANALYST", "acme", 1)
            .unwrap();
        let app = Router::new()
            .route(
                "/protected",
                post(|claims: crate::auth::AuthContext| async move {
                    Json(json!({
                        "user": claims.username,
                        "role": claims.role,
                        "tenant_id": claims.tenant_id,
                    }))
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(state, jwt_required));

        let ok = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/protected")
                    .header("authorization", format!("Bearer {token}"))
                    .header("X-Tenant-ID", "acme")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let bytes = to_bytes(ok.into_body(), 64 * 1024).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["tenant_id"], "acme");

        let forged = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/protected")
                    .header("authorization", format!("Bearer {token}"))
                    .header("X-Tenant-ID", "evil")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(forged.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn query_scope_guard_rejects_valid_token_without_sql_and_table_scopes() {
        let state = auth_state();
        let token = state
            .jwt
            .generate_token_with_scopes(
                7,
                "alice",
                "ANALYST",
                "acme",
                vec!["profile.read".to_string()],
                1,
            )
            .unwrap();
        let app = Router::new()
            .route("/query", post(|| async { Json(json!({ "status": "ok" })) }))
            .route_layer(axum::middleware::from_fn(require_query_scope))
            .route_layer(axum::middleware::from_fn_with_state(state, jwt_required));

        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/query")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn query_scope_guard_allows_sql_and_table_scope_or_admin_role() {
        let state = auth_state();
        let scoped = state
            .jwt
            .generate_token_with_scopes(
                7,
                "alice",
                "ANALYST",
                "acme",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let admin = state
            .jwt
            .generate_token_with_scopes(8, "root", "SYSADMIN", "acme", vec![], 1)
            .unwrap();
        let app = Router::new()
            .route("/query", post(|| async { Json(json!({ "status": "ok" })) }))
            .route_layer(axum::middleware::from_fn(require_query_scope))
            .route_layer(axum::middleware::from_fn_with_state(state, jwt_required));

        for token in [scoped, admin] {
            let resp = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri("/query")
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn option_b_owner_bootstrap_configures_workspace_identity_scim_storage_entitlement_and_audit_export()
     {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let catalog_path_string = catalog_path.to_string_lossy().to_string();
        Catalog::open(&catalog_path_string).unwrap();

        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = ClientRegistry::new();
        clients.register_with_metadata(
            "platform-owner",
            "secret",
            "ACCOUNTADMIN",
            "platform-root",
            vec!["*".to_string(), "break_glass.admin".to_string()],
        );
        clients.register_with_metadata(
            "platform-wildcard",
            "secret-wildcard",
            "ACCOUNTADMIN",
            "platform-root",
            vec!["*".to_string()],
        );
        let state = AuthState::new(jwt, clients, 1)
            .with_durable_service_catalog_path(catalog_path_string.clone());
        let platform_token = bearer_for_account(
            &state,
            "platform-owner",
            "ACCOUNTADMIN",
            "platform-root",
            vec!["*", "break_glass.admin"],
        );
        let wildcard_token = bearer_for_account(
            &state,
            "platform-wildcard",
            "ACCOUNTADMIN",
            "platform-root",
            vec!["*"],
        );
        let app = auth_router(state.clone());

        let wildcard_account = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/api/v1/accounts")
                    .header("authorization", format!("Bearer {wildcard_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"slug":"wildcard-denied"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wildcard_account.status(), StatusCode::FORBIDDEN);
        assert!(
            Catalog::open(&catalog_path_string)
                .unwrap()
                .get_control_plane_resource("account", "wildcard-denied")
                .unwrap()
                .is_none()
        );

        let account = app.clone().oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/accounts")
                .header("authorization", format!("Bearer {platform_token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"slug":"acme-corp","legal_name":"Acme Corp","owner_email":"owner@acme.example"}).to_string()))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(account.status(), StatusCode::OK);
        let account_body = response_json(account).await;
        let account_id = account_body["account"]["id"].as_str().unwrap();
        assert_eq!(account_id, "acme-corp");

        let owner_token = bearer_for_account(
            &state,
            "acme-owner",
            "ACCOUNTADMIN",
            account_id,
            vec!["policy.admin"],
        );
        let org = app.clone().oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/accounts/{account_id}/organizations"))
                .header("authorization", format!("Bearer {owner_token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"slug":"data","display_name":"Acme Data","verified_domains":["acme.example"]}).to_string()))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(org.status(), StatusCode::OK);
        let org_body = response_json(org).await;
        let org_id = org_body["organization"]["id"].as_str().unwrap();

        for (uri, payload, resource_type) in [
            (
                format!("/api/v1/organizations/{org_id}/idp-connections"),
                json!({"kind":"oidc","issuer":"https://idp.acme.example","client_id":"opensnow","client_secret":"plaintext"}),
                "idp_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/idp-connections"),
                json!({"kind":"oidc","issuer":"https://idp.acme.example","client_id":"opensnow","api_key":"plaintext"}),
                "idp_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/idp-connections"),
                json!({"kind":"oidc","issuer":"https://idp.acme.example","client_id":"opensnow","clientSecret":"plaintext"}),
                "idp_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/idp-connections"),
                json!({"kind":"oidc","issuer":"https://idp.acme.example","client_id":"opensnow","privateKey":"plaintext"}),
                "idp_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/scim-connections"),
                json!({"label":"okta","token":"plaintext"}),
                "scim_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/scim-connections"),
                json!({"label":"okta","secret_key":"plaintext"}),
                "scim_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/audit/export"),
                json!({"destination":"s3://acme-audit/opensnow","access_key":"plaintext"}),
                "audit_export",
            ),
            (
                format!("/api/v1/organizations/{org_id}/audit/export"),
                json!({"destination":"s3://acme-audit/opensnow","accessKey":"plaintext"}),
                "audit_export",
            ),
        ] {
            let before = Catalog::open(&catalog_path_string)
                .unwrap()
                .list_control_plane_resources(account_id, Some(org_id), None, Some(resource_type))
                .unwrap()
                .len();
            let denied = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(uri)
                        .header("authorization", format!("Bearer {owner_token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(payload.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
            let after = Catalog::open(&catalog_path_string)
                .unwrap()
                .list_control_plane_resources(account_id, Some(org_id), None, Some(resource_type))
                .unwrap()
                .len();
            assert_eq!(after, before);
        }

        for (uri, payload, expected_key) in [
            (
                format!("/api/v1/organizations/{org_id}/idp-connections"),
                json!({"kind":"oidc","issuer":"https://idp.acme.example","client_id":"opensnow","client_secret_handle":"secret://oidc","allowed_domains":["acme.example"]}),
                "idp_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/scim-connections"),
                json!({"label":"okta","token_secret_handle":"secret://scim"}),
                "scim_connection",
            ),
            (
                format!("/api/v1/organizations/{org_id}/audit/export"),
                json!({"destination":"s3://acme-audit/opensnow","format":"jsonl","secret_handle":"secret://audit"}),
                "audit_export",
            ),
            (
                format!("/api/v1/organizations/{org_id}/entitlement-bindings"),
                json!({"provider":"aws","entitlement_id":"ent-acme","features":["account.activate","warehouse.activate"]}),
                "entitlement_binding",
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(uri)
                        .header("authorization", format!("Bearer {owner_token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(payload.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(response_json(resp).await.get(expected_key).is_some());
        }

        let workspace = app.clone().oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/organizations/{org_id}/workspaces"))
                .header("authorization", format!("Bearer {owner_token}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"slug":"prod","deployment_mode":"aws_marketplace","warehouse_namespace":"acme_prod"}).to_string()))
                .unwrap(),
        ).await.unwrap();
        assert_eq!(workspace.status(), StatusCode::OK);
        let workspace_body = response_json(workspace).await;
        let workspace_id = workspace_body["workspace"]["id"].as_str().unwrap();

        for (uri, payload, expected_key) in [
            (
                format!("/api/v1/workspaces/{workspace_id}/object-storage-bindings"),
                json!({"provider":"aws","bucket":"acme-prod","prefix":"warehouse/","secret_handle":"secret://storage"}),
                "object_storage_binding",
            ),
            (
                format!("/api/v1/workspaces/{workspace_id}/secret-handles"),
                json!({"handle":"secret://warehouse","purpose":"object_storage"}),
                "secret_handle",
            ),
            (
                format!("/api/v1/workspaces/{workspace_id}/warehouse-bindings"),
                json!({"warehouse_name":"prod_wh","size":"medium"}),
                "warehouse_binding",
            ),
            (
                format!("/api/v1/workspaces/{workspace_id}/service-clients"),
                json!({"client_id":"svc-prod-loader","client_secret":"secret-v1","role":"ANALYST","scopes":["sql.query","table.select"]}),
                "client",
            ),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(uri)
                        .header("authorization", format!("Bearer {owner_token}"))
                        .header("content-type", "application/json")
                        .body(Body::from(payload.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            assert!(response_json(resp).await.get(expected_key).is_some());
        }

        let beta_token = bearer_for_account(
            &state,
            "beta-owner",
            "ACCOUNTADMIN",
            "beta-inc",
            vec!["policy.admin"],
        );
        let denied = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/v1/workspaces/{workspace_id}/warehouse-bindings"
                    ))
                    .header("authorization", format!("Bearer {beta_token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"warehouse_name":"forged"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let audit = app
            .oneshot(
                HttpRequest::builder()
                    .method("GET")
                    .uri(format!("/api/v1/organizations/{org_id}/audit/export"))
                    .header("authorization", format!("Bearer {owner_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(audit.status(), StatusCode::OK);
        let audit_body = response_json(audit).await;
        assert!(
            audit_body["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["action"] == "control_plane.warehouse_binding.create")
        );
        assert!(
            audit_body["events"]
                .as_array()
                .unwrap()
                .iter()
                .any(|event| event["decision"] == "denied")
        );
    }
}
