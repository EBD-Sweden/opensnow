use crate::jwt::JwtManager;
use crate::users::User;
use serde::{Deserialize, Serialize};

/// Authentication configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    /// Whether authentication is enabled. Disabled by default for localhost mode.
    pub enabled: bool,
    /// JWT secret for token signing/validation.
    pub jwt_secret: String,
    /// Default token expiry in hours.
    pub token_expiry_hours: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            jwt_secret: String::new(),
            token_expiry_hours: 24,
        }
    }
}

/// Extract a user from a Bearer token in an HTTP Authorization header value.
///
/// The `auth_header` should be the full value, e.g. `"Bearer eyJ..."`.
/// Returns `None` if the header is missing/malformed or the token is invalid.
pub fn extract_user_from_request(auth_header: Option<&str>, jwt: &JwtManager) -> Option<User> {
    let header = auth_header?;
    let token = header.strip_prefix("Bearer ")?;
    let claims = jwt.validate_token(token).ok()?;
    Some(User {
        id: claims.user_id,
        username: claims.username,
        role: claims.role,
    })
}

/// Helper for pgwire authentication: validate a username/password pair.
/// Returns the authenticated user or an error.
pub fn pgwire_authenticate(
    store: &crate::users::UserStore,
    username: &str,
    password: &str,
) -> anyhow::Result<User> {
    store.authenticate(username, password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_config_default_does_not_provide_a_signing_secret() {
        let config = AuthConfig::default();

        assert!(!config.enabled);
        assert!(
            config.jwt_secret.is_empty(),
            "disabled auth defaults must not ship a reusable JWT signing secret"
        );
        let prior_placeholder = ["opensnow", "default", "secret", "change", "me"].join("-");
        assert_ne!(config.jwt_secret, prior_placeholder);
    }

    #[test]
    fn test_extract_user_from_bearer() {
        let jwt = JwtManager::new(b"test-secret");
        let token = jwt.generate_token(1, "alice", "SYSADMIN", 24).unwrap();
        let header = format!("Bearer {}", token);

        let user = extract_user_from_request(Some(&header), &jwt).unwrap();
        assert_eq!(user.username, "alice");
        assert_eq!(user.role, "SYSADMIN");
    }

    #[test]
    fn test_extract_user_no_header() {
        let jwt = JwtManager::new(b"test-secret");
        assert!(extract_user_from_request(None, &jwt).is_none());
    }

    #[test]
    fn test_extract_user_invalid_token() {
        let jwt = JwtManager::new(b"test-secret");
        assert!(extract_user_from_request(Some("Bearer invalid"), &jwt).is_none());
    }

    #[test]
    fn test_extract_user_wrong_prefix() {
        let jwt = JwtManager::new(b"test-secret");
        let token = jwt.generate_token(1, "bob", "PUBLIC", 24).unwrap();
        let header = format!("Basic {}", token);
        assert!(extract_user_from_request(Some(&header), &jwt).is_none());
    }
}
