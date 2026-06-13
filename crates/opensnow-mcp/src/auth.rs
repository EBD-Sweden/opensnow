/// MCP bearer-token authentication middleware.
///
/// In dev mode (no `MCP_AUTH_TOKEN` env var set), all requests are allowed.
/// In production, callers must pass `Authorization: Bearer <token>`.
///
/// Role filtering is layered on top: the token is looked up in a simple
/// in-memory role map. Only "admin" and "analyst" roles are allowed to call
/// write-class tools (`safe_run_sql`, `propose_migration`).
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    body::Body,
    extract::{Extension, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::Response,
};
use opensnow_auth::{Claims, ExternalIdpConfig, ExternalIdpVerifier, PrivilegeStore};
use rusqlite::Connection;
use tracing::warn;

/// Extract the bearer token from the Authorization header of any request.
pub fn extract_bearer(req: &Request<Body>) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

fn jwt_secret_from_env() -> Option<String> {
    std::env::var("MCP_JWT_SECRET")
        .ok()
        .filter(|secret| !secret.is_empty())
}

#[derive(Clone)]
pub struct AuthConfig {
    jwt_secret: Option<String>,
    auth_token: Option<String>,
    roles: RoleMap,
    object_policy: Option<Arc<PrivilegeStore>>,
    /// Optional external IdP verifier (OAuth 2.x / OIDC). When set, bearer tokens
    /// issued by the org's IdP are accepted in addition to the modes above.
    external_idp: Option<Arc<ExternalIdpVerifier>>,
}

impl AuthConfig {
    pub fn disabled() -> Self {
        Self {
            jwt_secret: None,
            auth_token: None,
            roles: RoleMap::empty(),
            object_policy: None,
            external_idp: None,
        }
    }

    pub fn jwt(secret: impl Into<String>) -> Self {
        let secret = secret.into();
        Self {
            jwt_secret: (!secret.is_empty()).then_some(secret),
            auth_token: None,
            roles: RoleMap::empty(),
            object_policy: None,
            external_idp: None,
        }
    }

    pub fn with_role_token(mut self, token: impl Into<String>, role: impl Into<String>) -> Self {
        self.roles.insert(token, role);
        self
    }

    pub fn from_env() -> Self {
        Self {
            jwt_secret: jwt_secret_from_env(),
            auth_token: std::env::var("MCP_AUTH_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
            roles: RoleMap::from_env(),
            object_policy: object_policy_from_env(),
            external_idp: external_idp_from_env(),
        }
    }

    pub fn jwt_mode_enabled(&self) -> bool {
        self.jwt_secret.is_some()
    }

    pub fn external_idp_enabled(&self) -> bool {
        self.external_idp.is_some()
    }

    pub fn can_write_token(&self, token: &str) -> bool {
        self.roles.can_write(token)
    }

    pub fn with_object_policy(mut self, policy: PrivilegeStore) -> Self {
        self.object_policy = Some(Arc::new(policy));
        self
    }

    pub fn with_external_idp(mut self, verifier: ExternalIdpVerifier) -> Self {
        self.external_idp = Some(Arc::new(verifier));
        self
    }

    pub fn object_policy(&self) -> Option<&PrivilegeStore> {
        self.object_policy.as_deref()
    }

    /// Validate the bearer token and return mapped [`Claims`] for modes that
    /// carry scope/role information (external IdP, then HS256 JWT). Returns
    /// `Ok(None)` for static-token / no-auth modes — those are gated by
    /// [`require_auth`] but carry no scopes, so per-tool RBAC does not apply.
    ///
    /// `Err(UNAUTHORIZED)` only when a claim-carrying mode is the sole configured
    /// auth and the token fails to validate.
    pub async fn authenticate(&self, headers: &HeaderMap) -> Result<Option<Claims>, StatusCode> {
        let token = bearer_from_headers(headers);

        if let Some(verifier) = &self.external_idp {
            if let Some(tok) = token
                && let Ok(claims) = verifier.verify(tok).await
            {
                return Ok(Some(claims));
            }
            // External IdP is configured but did not validate this token. Fall
            // through to other modes if any are configured; otherwise fail closed.
            if self.jwt_secret.is_none() && self.auth_token.is_none() {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }

        if let Some(secret) = self.jwt_secret.as_deref() {
            let claims = claims_from_headers(headers, secret)?;
            return Ok(Some(claims));
        }

        Ok(None)
    }
}

fn bearer_from_headers(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
}

/// Build an external IdP verifier from `MCP_OIDC_*` env vars, if configured.
///
/// - `MCP_OIDC_ISSUER` (required to enable) — token `iss`, used for OIDC discovery.
/// - `MCP_OIDC_AUDIENCE` — comma-separated accepted audiences (optional).
/// - `MCP_OIDC_JWKS_URL` — explicit JWKS URL (optional; otherwise discovered).
/// - `MCP_OIDC_JWKS_TTL_SECS` — JWKS cache TTL (optional, default 3600).
/// - `MCP_OIDC_DEFAULT_ROLE` — role for tokens with no role claim (default PUBLIC).
fn external_idp_from_env() -> Option<Arc<ExternalIdpVerifier>> {
    let issuer = std::env::var("MCP_OIDC_ISSUER")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    let audiences = std::env::var("MCP_OIDC_AUDIENCE")
        .ok()
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let jwks_uri = std::env::var("MCP_OIDC_JWKS_URL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let jwks_ttl = std::env::var("MCP_OIDC_JWKS_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(3600));
    let default_role = std::env::var("MCP_OIDC_DEFAULT_ROLE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "PUBLIC".to_string());

    Some(Arc::new(ExternalIdpVerifier::new(ExternalIdpConfig {
        issuer,
        audiences,
        jwks_uri,
        jwks_ttl,
        default_role,
    })))
}

fn object_policy_from_env() -> Option<Arc<PrivilegeStore>> {
    let path = std::env::var("MCP_PRIVILEGE_CATALOG_PATH")
        .ok()
        .or_else(|| std::env::var("OPENSNOW_AUTH_CATALOG_PATH").ok())
        .or_else(|| std::env::var("OPENSNOW_CATALOG_PATH").ok())
        .filter(|path| !path.trim().is_empty())?;
    let conn = Connection::open(path).ok()?;
    PrivilegeStore::new(Arc::new(Mutex::new(conn)))
        .ok()
        .map(Arc::new)
}

fn has_scope(claims: &Claims, scope: &str) -> bool {
    claims.scopes.iter().any(|s| s == scope || s == "*")
}

fn is_platform_admin(claims: &Claims) -> bool {
    matches!(claims.role.as_str(), "ACCOUNTADMIN" | "SYSADMIN")
        || has_scope(claims, "policy.admin")
        || has_scope(claims, "admin")
}

/// Claims-based equivalent of [`authorize_headers_with_config`]: true when the
/// claims are a platform admin, hold all of `all_scopes`, or hold any of
/// `any_scopes`. Used by the `/mcp` per-tool authorization path, which validates
/// the token once (possibly via an external IdP) and authorizes on the result.
pub(crate) fn claims_satisfy(claims: &Claims, all_scopes: &[&str], any_scopes: &[&str]) -> bool {
    is_platform_admin(claims)
        || (!all_scopes.is_empty() && all_scopes.iter().all(|scope| has_scope(claims, scope)))
        || (!any_scopes.is_empty() && any_scopes.iter().any(|scope| has_scope(claims, scope)))
}

pub(crate) fn claims_is_admin(claims: &Claims) -> bool {
    is_platform_admin(claims)
}

pub fn claims_from_headers(headers: &HeaderMap, secret: &str) -> Result<Claims, StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let mgr = opensnow_auth::JwtManager::new(secret.as_bytes());
    mgr.validate_token(token).map_err(|e| {
        warn!("MCP: JWT rejected during policy check: {e}");
        StatusCode::UNAUTHORIZED
    })
}

pub fn jwt_mode_enabled() -> bool {
    jwt_secret_from_env().is_some()
}

pub fn claims_from_headers_with_config(
    headers: &HeaderMap,
    config: &AuthConfig,
) -> Result<Option<Claims>, StatusCode> {
    config
        .jwt_secret
        .as_deref()
        .map(|secret| claims_from_headers(headers, secret))
        .transpose()
}

/// Enforce enterprise JWT scopes on MCP handlers when `MCP_JWT_SECRET` is set.
/// In legacy/static-token and no-auth dev modes this returns `Ok(())`; those
/// modes are still governed by `require_auth` plus the existing `RoleMap` checks.
pub fn authorize_headers(
    headers: &HeaderMap,
    all_scopes: &[&str],
    any_scopes: &[&str],
) -> Result<(), StatusCode> {
    authorize_headers_with_config(headers, &AuthConfig::from_env(), all_scopes, any_scopes)
}

pub fn authorize_headers_with_config(
    headers: &HeaderMap,
    config: &AuthConfig,
    all_scopes: &[&str],
    any_scopes: &[&str],
) -> Result<(), StatusCode> {
    let Some(secret) = config.jwt_secret.as_deref() else {
        return Ok(());
    };
    let claims = claims_from_headers(headers, secret)?;
    if is_platform_admin(&claims)
        || (!all_scopes.is_empty() && all_scopes.iter().all(|scope| has_scope(&claims, scope)))
        || (!any_scopes.is_empty() && any_scopes.iter().any(|scope| has_scope(&claims, scope)))
    {
        Ok(())
    } else {
        warn!(
            user = %claims.username,
            role = %claims.role,
            tenant = %claims.tenant_id,
            required_all = ?all_scopes,
            required_any = ?any_scopes,
            "MCP: forbidden by route authorization policy"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

/// Middleware: require a valid bearer token.
///
/// Two modes, checked in order:
///
/// * `MCP_JWT_SECRET` is set → the bearer token must be a valid JWT signed
///   with that secret (delegates to [`opensnow_auth::JwtManager`]). This is
///   the production path used together with the server's `/auth/token`
///   endpoint.
/// * `MCP_AUTH_TOKEN` is set → the bearer token must match it byte-for-byte
///   (legacy static-token mode for tests / dev).
/// * Neither is set → all requests pass through (dev mode).
pub async fn require_auth(
    Extension(config): Extension<AuthConfig>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // External IdP (OAuth 2.x / OIDC), when configured, is tried first. A token
    // that validates here passes; otherwise we fall through to the other modes
    // (so an org can run external + HS256 side by side), or fail closed if it is
    // the only configured mode.
    if let Some(verifier) = config.external_idp.clone() {
        if let Some(token) = extract_bearer(&req)
            && verifier.verify(&token).await.is_ok()
        {
            return Ok(next.run(req).await);
        }
        if config.jwt_secret.is_none() && config.auth_token.is_none() {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    if let Some(secret) = config.jwt_secret.as_deref() {
        return require_jwt(secret, req, next).await;
    }

    let expected = match config.auth_token.as_deref() {
        Some(t) => t,
        _ => return Ok(next.run(req).await),
    };

    match extract_bearer(&req) {
        Some(token) if token == expected => Ok(next.run(req).await),
        Some(tok) => {
            warn!("MCP: rejected token (len={})", tok.len());
            Err(StatusCode::UNAUTHORIZED)
        }
        None => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn require_jwt(secret: &str, req: Request<Body>, next: Next) -> Result<Response, StatusCode> {
    let Some(token) = extract_bearer(&req) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let mgr = opensnow_auth::JwtManager::new(secret.as_bytes());
    match mgr.validate_token(&token) {
        Ok(_claims) => Ok(next.run(req).await),
        Err(e) => {
            warn!("MCP: JWT rejected: {e}");
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

/// A simple role map: token → role name.
///
/// In production this should be backed by the catalog's `roles` table.
/// For the MCP layer we keep it lightweight (env-based for now).
#[derive(Clone)]
pub struct RoleMap {
    inner: HashMap<String, String>,
}

impl RoleMap {
    pub fn empty() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    pub fn insert(&mut self, token: impl Into<String>, role: impl Into<String>) {
        self.inner.insert(token.into(), role.into().to_lowercase());
    }

    /// Build a role map from env vars of the form `MCP_TOKEN_<ROLE>=<token>`.
    /// e.g. `MCP_TOKEN_ADMIN=secret123` → token "secret123" → role "admin".
    pub fn from_env() -> Self {
        let mut inner = HashMap::new();
        for (key, val) in std::env::vars() {
            if let Some(role) = key.strip_prefix("MCP_TOKEN_") {
                inner.insert(val, role.to_lowercase());
            }
        }
        Self { inner }
    }

    /// Return the role for a given token, or `"anonymous"` if unknown.
    pub fn role_for(&self, token: &str) -> &str {
        self.inner
            .get(token)
            .map(|s| s.as_str())
            .unwrap_or("anonymous")
    }

    /// True when the token's role is permitted to run write-class operations.
    pub fn can_write(&self, token: &str) -> bool {
        matches!(self.role_for(token), "admin" | "analyst")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_map_returns_anonymous_for_unknown_token() {
        let map = RoleMap {
            inner: HashMap::new(),
        };
        assert_eq!(map.role_for("bad_token"), "anonymous");
        assert!(!map.can_write("bad_token"));
    }

    #[test]
    fn role_map_allows_admin_write() {
        let mut inner = HashMap::new();
        inner.insert("tok_admin".to_string(), "admin".to_string());
        let map = RoleMap { inner };
        assert!(map.can_write("tok_admin"));
    }

    #[test]
    fn role_map_denies_readonly_write() {
        let mut inner = HashMap::new();
        inner.insert("tok_ro".to_string(), "readonly".to_string());
        let map = RoleMap { inner };
        assert!(!map.can_write("tok_ro"));
    }
}
