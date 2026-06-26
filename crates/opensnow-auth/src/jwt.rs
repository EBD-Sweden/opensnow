use std::collections::HashSet;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// JWT claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// User ID (database primary key).
    pub user_id: i64,
    /// Username or service identity id.
    pub username: String,
    /// Primary role.
    pub role: String,
    /// Tenant/organization this credential is allowed to act within.
    #[serde(default = "default_tenant")]
    pub tenant_id: String,
    /// OAuth/OIDC scopes carried by the product token.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Durable session id for user tokens minted from enterprise OIDC/SAML.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Authentication method that produced this token (`client_credentials`, `oidc`, etc.).
    #[serde(default)]
    pub auth_method: Option<String>,
    /// Production product-token issuer (`iss`). Required in enterprise mode.
    #[serde(default, rename = "iss", skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Production product-token audience (`aud`). Required in enterprise mode.
    #[serde(default, rename = "aud", skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// Issued-at timestamp (seconds since epoch).
    pub iat: i64,
    /// Expiration timestamp (seconds since epoch).
    pub exp: i64,
}

fn default_tenant() -> String {
    "default".to_string()
}

pub struct SsoSessionTokenRequest<'a> {
    pub user_id: i64,
    pub username: &'a str,
    pub role: &'a str,
    pub tenant_id: &'a str,
    pub scopes: Vec<String>,
    pub session_id: &'a str,
    pub expiry_hours: i64,
}

/// Public JWK metadata published for product-token verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonWebKey {
    pub kty: String,
    #[serde(rename = "use")]
    pub key_use: String,
    pub alg: String,
    pub kid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub e: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<String>,
}

impl JsonWebKey {
    pub fn rsa(kid: &str, n: &str, e: &str) -> Self {
        Self {
            kty: "RSA".to_string(),
            key_use: "sig".to_string(),
            alg: "RS256".to_string(),
            kid: kid.to_string(),
            n: Some(n.to_string()),
            e: Some(e.to_string()),
            crv: None,
            x: None,
            y: None,
        }
    }

    pub fn ec_p256(kid: &str, x: &str, y: &str) -> Self {
        Self {
            kty: "EC".to_string(),
            key_use: "sig".to_string(),
            alg: "ES256".to_string(),
            kid: kid.to_string(),
            n: None,
            e: None,
            crv: Some("P-256".to_string()),
            x: Some(x.to_string()),
            y: Some(y.to_string()),
        }
    }
}

#[derive(Clone)]
pub struct EnterpriseJwtKey {
    kid: String,
    algorithm: Algorithm,
    encoding_key: Option<EncodingKey>,
    decoding_key: DecodingKey,
    jwk: Option<JsonWebKey>,
}

impl EnterpriseJwtKey {
    pub fn from_pem(
        kid: &str,
        algorithm: Algorithm,
        private_key_pem: &[u8],
        public_key_pem: &[u8],
        jwk: Option<JsonWebKey>,
    ) -> Result<Self> {
        let encoding_key = match algorithm {
            Algorithm::RS256 => {
                EncodingKey::from_rsa_pem(private_key_pem).context("invalid RSA private key PEM")?
            }
            Algorithm::ES256 => EncodingKey::from_ec_pem(private_key_pem)
                .context("invalid ECDSA P-256 private key PEM")?,
            _ => bail!("enterprise JWT algorithm must be RS256 or ES256"),
        };
        Ok(Self {
            kid: kid.to_string(),
            algorithm,
            encoding_key: Some(encoding_key),
            decoding_key: decoding_key_from_pem(algorithm, public_key_pem)?,
            jwk,
        })
    }

    pub fn verify_only_from_pem(
        kid: &str,
        algorithm: Algorithm,
        public_key_pem: &[u8],
        jwk: Option<JsonWebKey>,
    ) -> Result<Self> {
        match algorithm {
            Algorithm::RS256 | Algorithm::ES256 => {}
            _ => bail!("enterprise JWT algorithm must be RS256 or ES256"),
        }
        Ok(Self {
            kid: kid.to_string(),
            algorithm,
            encoding_key: None,
            decoding_key: decoding_key_from_pem(algorithm, public_key_pem)?,
            jwk,
        })
    }
}

fn decoding_key_from_pem(algorithm: Algorithm, public_key_pem: &[u8]) -> Result<DecodingKey> {
    match algorithm {
        Algorithm::RS256 => {
            DecodingKey::from_rsa_pem(public_key_pem).context("invalid RSA public key PEM")
        }
        Algorithm::ES256 => {
            DecodingKey::from_ec_pem(public_key_pem).context("invalid ECDSA P-256 public key PEM")
        }
        _ => bail!("enterprise JWT algorithm must be RS256 or ES256"),
    }
}

pub struct EnterpriseJwtConfig {
    pub issuer: String,
    pub audience: String,
    pub active_key: EnterpriseJwtKey,
    pub verification_keys: Vec<EnterpriseJwtKey>,
    pub revoked_kids: Vec<String>,
}

#[derive(Clone)]
struct EnterpriseJwtIssuer {
    issuer: String,
    audience: String,
    active_kid: String,
    keys: Vec<EnterpriseJwtKey>,
    revoked_kids: HashSet<String>,
}

/// Manages JWT token creation and validation.
#[derive(Clone)]
pub struct JwtManager {
    mode: JwtMode,
}

#[derive(Clone)]
enum JwtMode {
    LocalHs256 {
        encoding_key: EncodingKey,
        decoding_key: DecodingKey,
    },
    Enterprise(EnterpriseJwtIssuer),
}

impl JwtManager {
    /// Create a new local/dev JWT manager with the given HMAC-SHA256 secret.
    ///
    /// Production enterprise deployments should use [`JwtManager::enterprise`]
    /// so product tokens carry issuer/audience claims, an asymmetric `kid`, and
    /// are verifiable through JWKS rotation metadata.
    pub fn new(secret: &[u8]) -> Self {
        Self {
            mode: JwtMode::LocalHs256 {
                encoding_key: EncodingKey::from_secret(secret),
                decoding_key: DecodingKey::from_secret(secret),
            },
        }
    }

    pub fn enterprise(config: EnterpriseJwtConfig) -> Result<Self> {
        if config.issuer.trim().is_empty() {
            bail!("enterprise JWT issuer is required");
        }
        if config.audience.trim().is_empty() {
            bail!("enterprise JWT audience is required");
        }
        if config.active_key.encoding_key.is_none() {
            bail!("enterprise active JWT key must include private key material");
        }
        let active_kid = config.active_key.kid.clone();
        let mut keys = vec![config.active_key];
        keys.extend(config.verification_keys);
        let revoked_kids = config.revoked_kids.into_iter().collect();
        Ok(Self {
            mode: JwtMode::Enterprise(EnterpriseJwtIssuer {
                issuer: config.issuer,
                audience: config.audience,
                active_kid,
                keys,
                revoked_kids,
            }),
        })
    }

    pub fn with_revoked_kids(mut self, revoked_kids: Vec<String>) -> Self {
        if let JwtMode::Enterprise(issuer) = &mut self.mode {
            issuer.revoked_kids = revoked_kids.into_iter().collect();
        }
        self
    }

    /// Generate a signed JWT token for the default local tenant.
    pub fn generate_token(
        &self,
        user_id: i64,
        username: &str,
        role: &str,
        expiry_hours: i64,
    ) -> Result<String> {
        self.generate_token_for_tenant(user_id, username, role, "default", expiry_hours)
    }

    /// Generate a signed JWT token bound to a tenant/organization.
    pub fn generate_token_for_tenant(
        &self,
        user_id: i64,
        username: &str,
        role: &str,
        tenant_id: &str,
        expiry_hours: i64,
    ) -> Result<String> {
        self.generate_token_with_scopes(
            user_id,
            username,
            role,
            tenant_id,
            vec![role.to_string()],
            expiry_hours,
        )
    }

    /// Generate a signed JWT token bound to a tenant/organization and explicit scopes.
    pub fn generate_token_with_scopes(
        &self,
        user_id: i64,
        username: &str,
        role: &str,
        tenant_id: &str,
        scopes: Vec<String>,
        expiry_hours: i64,
    ) -> Result<String> {
        match &self.mode {
            JwtMode::LocalHs256 { .. } => self.generate_token_inner(
                user_id,
                username,
                role,
                tenant_id,
                scopes,
                expiry_hours,
                None,
                None,
                None,
                Some("client_credentials"),
            ),
            JwtMode::Enterprise(issuer) => self.generate_token_inner(
                user_id,
                username,
                role,
                tenant_id,
                scopes,
                expiry_hours,
                Some(&issuer.issuer),
                Some(&issuer.audience),
                None,
                Some("client_credentials"),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn generate_token_with_issuer_audience(
        &self,
        user_id: i64,
        username: &str,
        role: &str,
        tenant_id: &str,
        scopes: Vec<String>,
        expiry_hours: i64,
        issuer: &str,
        audience: &str,
    ) -> Result<String> {
        self.generate_token_inner(
            user_id,
            username,
            role,
            tenant_id,
            scopes,
            expiry_hours,
            Some(issuer),
            Some(audience),
            None,
            Some("client_credentials"),
        )
    }

    /// Generate a scoped product token tied to a durable enterprise SSO session.
    pub fn generate_sso_session_token(&self, req: SsoSessionTokenRequest<'_>) -> Result<String> {
        let (issuer, audience) = match &self.mode {
            JwtMode::LocalHs256 { .. } => (None, None),
            JwtMode::Enterprise(enterprise) => (
                Some(enterprise.issuer.as_str()),
                Some(enterprise.audience.as_str()),
            ),
        };
        self.generate_token_inner(
            req.user_id,
            req.username,
            req.role,
            req.tenant_id,
            req.scopes,
            req.expiry_hours,
            issuer,
            audience,
            Some(req.session_id),
            Some("oidc"),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn generate_token_inner(
        &self,
        user_id: i64,
        username: &str,
        role: &str,
        tenant_id: &str,
        scopes: Vec<String>,
        expiry_hours: i64,
        issuer: Option<&str>,
        audience: Option<&str>,
        session_id: Option<&str>,
        auth_method: Option<&str>,
    ) -> Result<String> {
        let now = Utc::now().timestamp();
        let claims = Claims {
            user_id,
            username: username.to_string(),
            role: role.to_string(),
            tenant_id: tenant_id.to_string(),
            scopes,
            session_id: session_id.map(ToString::to_string),
            auth_method: auth_method.map(ToString::to_string),
            issuer: issuer.map(ToString::to_string),
            audience: audience.map(ToString::to_string),
            iat: now,
            exp: now + expiry_hours * 3600,
        };

        match &self.mode {
            JwtMode::LocalHs256 { encoding_key, .. } => {
                jsonwebtoken::encode(&Header::default(), &claims, encoding_key)
                    .context("failed to encode JWT")
            }
            JwtMode::Enterprise(issuer) => {
                if issuer.revoked_kids.contains(&issuer.active_kid) {
                    bail!("active enterprise JWT key is revoked");
                }
                let active_key = issuer
                    .keys
                    .iter()
                    .find(|key| key.kid == issuer.active_kid)
                    .ok_or_else(|| anyhow!("active enterprise JWT key is not configured"))?;
                let encoding_key = active_key
                    .encoding_key
                    .as_ref()
                    .ok_or_else(|| anyhow!("active enterprise JWT key is verify-only"))?;
                let mut header = Header::new(active_key.algorithm);
                header.kid = Some(active_key.kid.clone());
                jsonwebtoken::encode(&header, &claims, encoding_key)
                    .context("failed to encode enterprise JWT")
            }
        }
    }

    /// Validate a JWT token and return the claims.
    pub fn validate_token(&self, token: &str) -> Result<Claims> {
        match &self.mode {
            JwtMode::LocalHs256 { decoding_key, .. } => {
                let mut validation = Validation::new(Algorithm::HS256);
                validation.validate_exp = true;
                let token_data: TokenData<Claims> =
                    jsonwebtoken::decode(token, decoding_key, &validation)
                        .context("invalid or expired JWT")?;
                Ok(token_data.claims)
            }
            JwtMode::Enterprise(issuer) => {
                let header = jsonwebtoken::decode_header(token).context("invalid JWT header")?;
                let kid = header
                    .kid
                    .as_deref()
                    .ok_or_else(|| anyhow!("enterprise JWT missing kid"))?;
                if issuer.revoked_kids.contains(kid) {
                    bail!("enterprise JWT kid is revoked");
                }
                let key = issuer
                    .keys
                    .iter()
                    .find(|key| key.kid == kid && key.algorithm == header.alg)
                    .ok_or_else(|| anyhow!("enterprise JWT kid is not trusted"))?;
                let mut validation = Validation::new(key.algorithm);
                validation.validate_exp = true;
                validation.set_issuer(&[issuer.issuer.as_str()]);
                validation.set_audience(&[issuer.audience.as_str()]);
                let token_data: TokenData<Claims> =
                    jsonwebtoken::decode(token, &key.decoding_key, &validation)
                        .context("invalid enterprise JWT")?;
                Ok(token_data.claims)
            }
        }
    }

    /// Publish non-revoked enterprise verification keys as a JWKS document.
    pub fn jwks(&self) -> Option<Value> {
        match &self.mode {
            JwtMode::LocalHs256 { .. } => None,
            JwtMode::Enterprise(issuer) => {
                let keys: Vec<Value> = issuer
                    .keys
                    .iter()
                    .filter(|key| !issuer.revoked_kids.contains(&key.kid))
                    .filter_map(|key| key.jwk.as_ref())
                    .map(|jwk| serde_json::to_value(jwk).expect("JWK serializes"))
                    .collect();
                Some(json!({ "keys": keys }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::Algorithm;
    use openssl::rsa::Rsa;

    fn b64url(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn rsa_test_material() -> (Vec<u8>, Vec<u8>, String, String) {
        let key = Rsa::generate(2048).unwrap();
        let private_pem = key.private_key_to_pem().unwrap();
        let public_pem = key.public_key_to_pem().unwrap();
        let n = b64url(&key.n().to_vec());
        let e = b64url(&key.e().to_vec());
        (private_pem, public_pem, n, e)
    }

    fn enterprise_rs256_manager(kid: &str) -> JwtManager {
        let (private_pem, public_pem, n, e) = rsa_test_material();
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
    fn test_jwt_roundtrip() {
        let mgr = JwtManager::new(b"test-secret-key-12345");
        let token = mgr.generate_token(42, "alice", "SYSADMIN", 24).unwrap();

        let claims = mgr.validate_token(&token).unwrap();
        assert_eq!(claims.user_id, 42);
        assert_eq!(claims.username, "alice");
        assert_eq!(claims.role, "SYSADMIN");
        assert!(claims.exp > claims.iat);
        assert!(claims.issuer.is_none());
        assert!(claims.audience.is_none());
    }

    #[test]
    fn test_jwt_roundtrip_with_tenant_and_scopes() {
        let mgr = JwtManager::new(b"test-secret-key-12345");
        let token = mgr
            .generate_token_with_scopes(
                0,
                "svc-ci",
                "ANALYST",
                "org-acme",
                vec!["sql.query".to_string(), "table.select".to_string()],
                24,
            )
            .unwrap();

        let claims = mgr.validate_token(&token).unwrap();
        assert_eq!(claims.username, "svc-ci");
        assert_eq!(claims.role, "ANALYST");
        assert_eq!(claims.tenant_id, "org-acme");
        assert_eq!(claims.scopes, vec!["sql.query", "table.select"]);
    }

    #[test]
    fn test_invalid_token() {
        let mgr = JwtManager::new(b"secret-a");
        let token = mgr.generate_token(1, "bob", "PUBLIC", 1).unwrap();

        // Different secret should fail
        let mgr2 = JwtManager::new(b"secret-b");
        assert!(mgr2.validate_token(&token).is_err());
    }

    #[test]
    fn test_expired_token() {
        let mgr = JwtManager::new(b"secret");
        // Generate token that expired 1 hour ago
        let token = mgr.generate_token(1, "bob", "PUBLIC", -2).unwrap();
        assert!(mgr.validate_token(&token).is_err());
    }

    #[test]
    fn enterprise_rs256_tokens_validate_issuer_audience_kid_and_publish_jwks() {
        let mgr = enterprise_rs256_manager("kid-active");
        let token = mgr.generate_token(42, "alice", "SYSADMIN", 1).unwrap();
        let header = jsonwebtoken::decode_header(&token).unwrap();

        assert_eq!(header.alg, Algorithm::RS256);
        assert_eq!(header.kid.as_deref(), Some("kid-active"));
        let claims = mgr.validate_token(&token).unwrap();
        assert_eq!(
            claims.issuer.as_deref(),
            Some("https://opensnow.example/auth")
        );
        assert_eq!(claims.audience.as_deref(), Some("opensnow-api"));

        let jwks = mgr.jwks().expect("enterprise issuer publishes JWKS");
        assert_eq!(jwks["keys"][0]["kid"], "kid-active");
        assert_eq!(jwks["keys"][0]["alg"], "RS256");
        assert_eq!(jwks["keys"][0]["use"], "sig");
    }

    #[test]
    fn enterprise_rs256_validation_fails_closed_for_wrong_issuer_audience_unknown_kid_expired_and_revoked_key()
     {
        let trusted = enterprise_rs256_manager("kid-active");
        let wrong_issuer = enterprise_rs256_manager("kid-active");
        let wrong_audience = enterprise_rs256_manager("kid-active");
        let unknown_kid = enterprise_rs256_manager("kid-unknown");
        let revoked_base = enterprise_rs256_manager("kid-revoked");
        let revoked_token = revoked_base
            .generate_token(1, "alice", "SYSADMIN", 1)
            .unwrap();
        let revoked = revoked_base.with_revoked_kids(vec!["kid-revoked".to_string()]);

        assert!(
            trusted
                .validate_token(
                    &wrong_issuer
                        .generate_token_with_issuer_audience(
                            1,
                            "alice",
                            "SYSADMIN",
                            "default",
                            vec!["SYSADMIN".to_string()],
                            1,
                            "https://evil.example",
                            "opensnow-api",
                        )
                        .unwrap(),
                )
                .is_err()
        );
        assert!(
            trusted
                .validate_token(
                    &wrong_audience
                        .generate_token_with_issuer_audience(
                            1,
                            "alice",
                            "SYSADMIN",
                            "default",
                            vec!["SYSADMIN".to_string()],
                            1,
                            "https://opensnow.example/auth",
                            "other-api",
                        )
                        .unwrap(),
                )
                .is_err()
        );
        assert!(
            trusted
                .validate_token(
                    &unknown_kid
                        .generate_token(1, "alice", "SYSADMIN", 1)
                        .unwrap(),
                )
                .is_err()
        );
        assert!(
            trusted
                .validate_token(&trusted.generate_token(1, "alice", "SYSADMIN", -2).unwrap())
                .is_err()
        );
        assert!(revoked.validate_token(&revoked_token).is_err());
        assert_eq!(revoked.jwks().unwrap()["keys"].as_array().unwrap().len(), 0);
    }
}
