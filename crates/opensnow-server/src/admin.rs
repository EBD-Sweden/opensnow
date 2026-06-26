use std::path::{Path as StdPath, PathBuf};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{delete, get, post, put},
};
use opensnow_auth::{IdpConnectionUpsert, SsoManager, SsoProtocol, SsoSessionTokenRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize)]
pub struct TenantRequest {
    /// Account/org that owns this IdP connection. Kept optional so the original
    /// invite-only/local API shape can still use `slug` as the account id.
    pub account_id: Option<String>,
    pub slug: String,
    pub name: String,
    pub sso_enabled: Option<bool>,
    pub protocol: Option<SsoProtocol>,
    pub oidc_issuer: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_client_secret: Option<String>,
    pub oidc_client_secret_handle: Option<String>,
    pub allowed_domains: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
pub struct SsoMappingRequest {
    pub connection_id: Option<String>,
    pub idp_claim_key: String,
    pub idp_claim_value: String,
    pub role_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct RoleRequest {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct UserRolesRequest {
    pub role_ids: Vec<String>,
}

#[derive(Deserialize)]
pub struct SsoLoginRequest {
    pub email: Option<String>,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
}

#[derive(Deserialize)]
pub struct SsoCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Build admin routes backed by SsoManager durable SQLite storage. The routes
/// are still protected by the caller's JWT admin layer in auth-enabled mode.
pub fn admin_router(manager: SsoManager) -> Router {
    Router::new()
        .route(
            "/api/v1/admin/tenants",
            get(list_tenants).post(upsert_tenant),
        )
        .route(
            "/api/v1/admin/tenants/{tenant_id}/sso-mappings",
            get(list_sso_mappings).post(create_sso_mapping),
        )
        .route(
            "/api/v1/admin/tenants/{tenant_id}/sso-mappings/{mapping_id}",
            delete(delete_sso_mapping),
        )
        .route("/api/v1/admin/roles", get(list_roles).post(create_role))
        .route(
            "/api/v1/admin/roles/{role_id}",
            get(get_role).put(update_role),
        )
        .route("/api/v1/admin/users", get(list_users))
        .route(
            "/api/v1/admin/users/{user_id}/roles",
            put(update_user_roles),
        )
        .with_state(manager)
}

#[derive(Clone)]
struct AuthLoginState {
    manager: SsoManager,
    auth_state: Option<crate::auth::AuthState>,
}

/// Public auth/login initiation routes. Login stays public, but returns only an
/// OIDC authorization redirect descriptor. SAML fails closed with an explicit
/// unsupported response until a brokered metadata/ACS profile is implemented.
pub fn auth_login_router(
    manager: SsoManager,
    auth_state: Option<crate::auth::AuthState>,
) -> Router {
    Router::new()
        .route("/api/v1/auth/sso/login", post(sso_login))
        .route("/api/v1/auth/sso/callback", get(sso_callback))
        .with_state(AuthLoginState {
            manager,
            auth_state,
        })
}

pub fn default_sso_manager() -> SsoManager {
    let path = default_sso_db_path();
    open_durable_sso_manager(path).expect(
        "failed to open durable SSO catalog; set OPENSNOW_SSO_DB_PATH to a writable SQLite path",
    )
}

/// Resolve the SSO session database path. Defaults to an application-owned
/// location under `$HOME/.opensnow` rather than a world-readable temp dir, so
/// session secrets are not exposed to other local users.
fn default_sso_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("OPENSNOW_SSO_DB_PATH") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let dir = base.join(".opensnow");
    // Best-effort: create the directory with owner-only permissions.
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir.join("sso.sqlite")
}

fn open_durable_sso_manager(path: impl AsRef<StdPath>) -> anyhow::Result<SsoManager> {
    SsoManager::open(path)
}

const DEFAULT_SSO_REDIRECT_URI: &str = "/api/v1/auth/sso/callback";

fn configured_sso_redirect_uri() -> String {
    std::env::var("OPENSNOW_SSO_REDIRECT_URI").unwrap_or_else(|_| DEFAULT_SSO_REDIRECT_URI.into())
}

fn validated_sso_redirect_uri(requested: Option<String>) -> Result<String, String> {
    let configured = configured_sso_redirect_uri();
    let candidate = requested.unwrap_or_else(|| configured.clone());
    if candidate == configured {
        Ok(candidate)
    } else {
        Err(format!(
            "redirect_uri must match the configured SSO callback {configured}"
        ))
    }
}

async fn list_tenants(State(manager): State<SsoManager>) -> Json<Value> {
    // This compatibility endpoint lists account-owned IdP connections. Account
    // scoping is explicit on upsert; listing all accounts is intentionally not
    // exposed here to avoid accidental cross-account enumeration.
    let account_id =
        std::env::var("OPENSNOW_DEFAULT_ACCOUNT_ID").unwrap_or_else(|_| "default".into());
    match manager.list_idp_connections(&account_id) {
        Ok(connections) => Json(
            json!({ "status": "ok", "account_id": account_id, "idp_connections": connections }),
        ),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

async fn upsert_tenant(
    State(manager): State<SsoManager>,
    Json(req): Json<TenantRequest>,
) -> Json<Value> {
    let account_id = req.account_id.clone().unwrap_or_else(|| req.slug.clone());
    let protocol = req.protocol.unwrap_or(SsoProtocol::Oidc);
    let issuer = req.oidc_issuer.unwrap_or_default();
    let client_id = req.oidc_client_id.unwrap_or_default();
    let allowed_domains = req.allowed_domains.unwrap_or_else(|| {
        req.name
            .rsplit_once('@')
            .map(|(_, d)| vec![d.to_string()])
            .unwrap_or_default()
    });
    match manager.upsert_idp_connection(IdpConnectionUpsert {
        account_id: account_id.clone(),
        connection_id: req.slug,
        protocol,
        enabled: req.sso_enabled.unwrap_or(true),
        issuer,
        client_id,
        client_secret: req.oidc_client_secret,
        client_secret_handle: req.oidc_client_secret_handle,
        allowed_domains,
        scopes: None,
    }) {
        Ok(conn) => Json(json!({ "status": "ok", "idp_connection": conn.redacted() })),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

async fn list_sso_mappings(
    Path(tenant_id): Path<String>,
    State(manager): State<SsoManager>,
) -> Json<Value> {
    match manager.list_idp_role_mappings(&tenant_id) {
        Ok(mappings) => {
            Json(json!({ "status": "ok", "account_id": tenant_id, "mappings": mappings }))
        }
        Err(e) => {
            Json(json!({ "status": "error", "account_id": tenant_id, "message": e.to_string() }))
        }
    }
}

async fn create_sso_mapping(
    Path(tenant_id): Path<String>,
    State(manager): State<SsoManager>,
    Json(req): Json<SsoMappingRequest>,
) -> Json<Value> {
    let connection_id = req.connection_id.unwrap_or_else(|| "default".into());
    match manager.upsert_idp_role_mapping(
        &tenant_id,
        &connection_id,
        &req.idp_claim_key,
        &req.idp_claim_value,
        &req.role_id,
    ) {
        Ok(mapping) => Json(json!({ "status": "ok", "mapping": mapping })),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

async fn delete_sso_mapping(
    Path((tenant_id, mapping_id)): Path<(String, String)>,
    State(manager): State<SsoManager>,
) -> Json<Value> {
    match manager.delete_idp_role_mapping(&tenant_id, &mapping_id) {
        Ok(deleted) => {
            Json(json!({ "status": "ok", "mapping_id": mapping_id, "deleted": deleted }))
        }
        Err(e) => {
            Json(json!({ "status": "error", "mapping_id": mapping_id, "message": e.to_string() }))
        }
    }
}

async fn list_roles() -> Json<Value> {
    Json(json!({ "roles": ["PUBLIC", "SYSADMIN", "SECURITYADMIN", "ACCOUNTADMIN"] }))
}

async fn create_role(Json(req): Json<RoleRequest>) -> Json<Value> {
    Json(
        json!({ "status": "error", "error": "custom_role_api_not_enabled", "id": req.name, "message": "custom role persistence is not wired to durable RBAC storage yet; use built-in roles or SCIM group assignment" }),
    )
}

async fn get_role(Path(role_id): Path<String>) -> Json<Value> {
    Json(json!({ "id": role_id, "name": role_id, "description": null }))
}

async fn update_role(Path(role_id): Path<String>, Json(req): Json<RoleRequest>) -> Json<Value> {
    Json(
        json!({ "status": "error", "error": "custom_role_api_not_enabled", "id": role_id, "name": req.name, "message": "custom role updates are not wired to durable RBAC storage yet; request failed closed" }),
    )
}

async fn list_users() -> Json<Value> {
    Json(
        json!({ "users": Vec::<Value>::new(), "message": "SSO users are materialized on validated login" }),
    )
}

async fn update_user_roles(
    Path(user_id): Path<String>,
    Json(req): Json<UserRolesRequest>,
) -> Json<Value> {
    Json(
        json!({ "status": "error", "error": "manual_user_role_api_not_enabled", "user_id": user_id, "role_ids": req.role_ids, "message": "manual user-role updates are not wired to durable RBAC storage yet; use SCIM group assignment or validated SSO role mappings" }),
    )
}

async fn sso_login(
    State(state): State<AuthLoginState>,
    Json(req): Json<SsoLoginRequest>,
) -> Json<Value> {
    let manager = &state.manager;
    let Some(email) = req.email else {
        return Json(json!({ "status": "error", "error": "email_required" }));
    };
    let redirect_uri = match validated_sso_redirect_uri(req.redirect_uri) {
        Ok(uri) => uri,
        Err(message) => {
            return Json(json!({
                "status": "error",
                "error": "invalid_sso_redirect_uri",
                "message": message
            }));
        }
    };
    if req.code.is_some() {
        return Json(json!({
            "status": "error",
            "error": "oidc_callback_not_enabled",
            "durable_session_created": false,
            "message": "OpenSnow embedded SSO can initiate OIDC with state, nonce, and PKCE, but backend code exchange/session token minting is not enabled; raw authorization codes fail closed"
        }));
    }
    match manager.find_oidc_connection_by_email(&email) {
        Ok(Some(conn)) if conn.protocol == SsoProtocol::Oidc => {
            match manager.start_oidc_login(&conn.account_id, &conn.id, &redirect_uri) {
                Ok(start) => Json(json!({
                    "status": "ok",
                    "protocol": "oidc",
                    "account_id": start.account_id,
                    "connection_id": start.connection_id,
                    "authorization_url": start.authorization_url,
                    "state": start.state,
                    "message": "OIDC login initiated with state, nonce, and PKCE. Client secret is held by secret handle only."
                })),
                Err(e) => Json(
                    json!({ "status": "error", "error": "oidc_login_failed", "message": e.to_string() }),
                ),
            }
        }
        Ok(Some(conn)) if conn.protocol == SsoProtocol::Saml => Json(json!({
            "status": "error",
            "error": "saml_unsupported_fail_closed",
            "account_id": conn.account_id,
            "connection_id": conn.id,
            "message": "SAML metadata/ACS is not implemented in embedded OpenSnow enterprise auth; use OIDC or an external broker."
        })),
        Ok(Some(_)) => Json(json!({ "status": "error", "error": "unsupported_sso_protocol" })),
        Ok(None) => Json(json!({
            "status": "error",
            "error": "sso_not_configured_for_domain",
            "email": email,
            "message": "No enabled account-owned IdP connection matched the verified email domain"
        })),
        Err(e) => Json(
            json!({ "status": "error", "error": "sso_lookup_failed", "message": e.to_string() }),
        ),
    }
}

async fn sso_callback(
    State(state): State<AuthLoginState>,
    Query(query): Query<SsoCallbackQuery>,
) -> Json<Value> {
    let manager = &state.manager;
    if let Some(error) = query.error {
        return Json(json!({
            "status": "error",
            "error": "oidc_provider_error",
            "provider_error": error,
            "durable_session_created": false
        }));
    }
    let Some(code) = query.code else {
        return Json(json!({
            "status": "error",
            "error": "oidc_code_required",
            "durable_session_created": false
        }));
    };
    let Some(oidc_state) = query.state else {
        return Json(json!({
            "status": "error",
            "error": "oidc_state_required",
            "durable_session_created": false
        }));
    };

    match manager.complete_oidc_code_login(&oidc_state, &code).await {
        Ok(session) => {
            let primary_role = session
                .roles
                .first()
                .cloned()
                .unwrap_or_else(|| "PUBLIC".to_string());
            let user_id = match manager.upsert_sso_user(
                &session.account_id,
                &session.email,
                None,
                &session.roles,
            ) {
                Ok(id) => id.parse::<i64>().unwrap_or(0),
                Err(e) => {
                    return Json(json!({
                        "status": "error",
                        "error": "sso_user_upsert_failed",
                        "message": e.to_string(),
                        "durable_session_created": false
                    }));
                }
            };
            let ttl_seconds = 8 * 3600;
            let session_id = match manager.create_sso_session(
                &session.account_id,
                &session.connection_id,
                &session.subject,
                &session.email,
                &session.roles,
                ttl_seconds,
            ) {
                Ok(id) => id,
                Err(e) => {
                    return Json(json!({
                        "status": "error",
                        "error": "sso_session_persist_failed",
                        "message": e.to_string(),
                        "durable_session_created": false
                    }));
                }
            };
            let Some(auth_state) = state.auth_state.as_ref() else {
                return Json(json!({
                    "status": "error",
                    "error": "product_jwt_config_required",
                    "message": "OpenSnow auth state is required to mint a scoped OIDC session token",
                    "durable_session_created": true
                }));
            };
            match auth_state
                .jwt
                .generate_sso_session_token(SsoSessionTokenRequest {
                    user_id,
                    username: &session.email,
                    role: &primary_role,
                    tenant_id: &session.account_id,
                    scopes: session.roles.clone(),
                    session_id: &session_id,
                    expiry_hours: 8,
                }) {
                Ok(access_token) => Json(json!({
                    "status": "ok",
                    "protocol": "oidc",
                    "token_type": "Bearer",
                    "access_token": access_token,
                    "expires_in": 8 * 3600,
                    "account_id": session.account_id,
                    "connection_id": session.connection_id,
                    "subject": session.subject,
                    "email": session.email,
                    "roles": session.roles,
                    "session_id": session_id,
                    "durable_session_created": true
                })),
                Err(e) => Json(json!({
                    "status": "error",
                    "error": "session_token_mint_failed",
                    "message": e.to_string(),
                    "durable_session_created": true
                })),
            }
        }
        Err(e) => Json(json!({
            "status": "error",
            "error": "oidc_callback_failed",
            "message": e.to_string(),
            "durable_session_created": false
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::Algorithm;
    use opensnow_auth::{EnterpriseJwtConfig, EnterpriseJwtKey, JsonWebKey, JwtManager};
    use openssl::rsa::Rsa;

    fn b64url(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn enterprise_rs256_manager(kid: &str) -> JwtManager {
        let key = Rsa::generate(2048).unwrap();
        let private_pem = key.private_key_to_pem().unwrap();
        let public_pem = key.public_key_to_pem().unwrap();
        let n = b64url(&key.n().to_vec());
        let e = b64url(&key.e().to_vec());
        JwtManager::enterprise(EnterpriseJwtConfig {
            issuer: "https://opensnow.example/auth".to_string(),
            audience: "opensnow-api".to_string(),
            active_key: EnterpriseJwtKey::from_pem(
                kid,
                Algorithm::RS256,
                &private_pem,
                &public_pem,
                Some(JsonWebKey::rsa(kid, &n, &e)),
            )
            .unwrap(),
            verification_keys: vec![],
            revoked_kids: vec![],
        })
        .unwrap()
    }

    #[test]
    fn sso_redirect_uri_must_match_configured_callback() {
        assert_eq!(
            validated_sso_redirect_uri(Some("/api/v1/auth/sso/callback".to_string())).unwrap(),
            "/api/v1/auth/sso/callback"
        );
        assert!(validated_sso_redirect_uri(None).is_ok());
        assert!(
            validated_sso_redirect_uri(Some("https://evil.example/callback".to_string())).is_err()
        );
        assert!(validated_sso_redirect_uri(Some("//evil.example/callback".to_string())).is_err());
    }

    #[test]
    fn durable_sso_manager_open_errors_instead_of_falling_back_to_memory() {
        let impossible_path = std::env::temp_dir()
            .join("opensnow-sso-missing-parent")
            .join("sso.sqlite");
        assert!(open_durable_sso_manager(impossible_path).is_err());
    }

    #[test]
    fn oidc_callback_token_minting_uses_enterprise_jwt_manager_claims() {
        let jwt = enterprise_rs256_manager("opensnow-prod-rs256");
        let token = jwt
            .generate_sso_session_token(SsoSessionTokenRequest {
                user_id: 42,
                username: "alice@example.com",
                role: "ANALYST",
                tenant_id: "acct_acme",
                scopes: vec!["ANALYST".to_string(), "sql.query".to_string()],
                session_id: "sso-session-1",
                expiry_hours: 1,
            })
            .unwrap();

        let claims = jwt.validate_token(&token).unwrap();
        assert_eq!(
            claims.issuer.as_deref(),
            Some("https://opensnow.example/auth")
        );
        assert_eq!(claims.audience.as_deref(), Some("opensnow-api"));
        assert_eq!(claims.auth_method.as_deref(), Some("oidc"));
        assert_eq!(claims.session_id.as_deref(), Some("sso-session-1"));
        assert_eq!(claims.username, "alice@example.com");
        assert_eq!(claims.tenant_id, "acct_acme");
        assert_eq!(claims.scopes, vec!["ANALYST", "sql.query"]);
    }
}
