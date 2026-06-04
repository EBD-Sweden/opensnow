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
};

use axum::{
    body::Body,
    extract::{Extension, Request},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::Response,
};
use opensnow_auth::{Claims, PrivilegeStore};
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
}

impl AuthConfig {
    pub fn disabled() -> Self {
        Self {
            jwt_secret: None,
            auth_token: None,
            roles: RoleMap::empty(),
            object_policy: None,
        }
    }

    pub fn jwt(secret: impl Into<String>) -> Self {
        let secret = secret.into();
        Self {
            jwt_secret: (!secret.is_empty()).then_some(secret),
            auth_token: None,
            roles: RoleMap::empty(),
            object_policy: None,
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
        }
    }

    pub fn jwt_mode_enabled(&self) -> bool {
        self.jwt_secret.is_some()
    }

    pub fn can_write_token(&self, token: &str) -> bool {
        self.roles.can_write(token)
    }

    pub fn with_object_policy(mut self, policy: PrivilegeStore) -> Self {
        self.object_policy = Some(Arc::new(policy));
        self
    }

    pub fn object_policy(&self) -> Option<&PrivilegeStore> {
        self.object_policy.as_deref()
    }
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
