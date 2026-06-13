//! External IdP token verification (OAuth 2.x / OpenID Connect).
//!
//! OpenSnow is self-hosted per organization. Beyond its own HS256 shared-secret
//! and RS256/ES256 enterprise tokens (see [`crate::jwt`]), an organization may
//! want to let their own apps/agents authenticate to OpenSnow using tokens
//! issued by *their* identity provider — Okta, Entra ID, Auth0, Keycloak, Google,
//! etc. This module validates such externally-issued JWT access / ID tokens:
//!
//! 1. Read the token header to find `kid` and `alg`.
//! 2. Resolve the IdP's JWKS (OIDC discovery `…/.well-known/openid-configuration`
//!    → `jwks_uri`, or a configured URL, with a cached TTL).
//! 3. Build a decoding key from the matching JWK (RSA `n`/`e` or EC `x`/`y`).
//! 4. Verify signature, `iss`, optional `aud`, and `exp`.
//! 5. Map standard OAuth/OIDC claims (`scope`/`scp`/`scopes`, `roles`/`role`,
//!    `preferred_username`/`email`/`sub`) onto OpenSnow [`Claims`], which the
//!    existing scope-based authorization already understands.
//!
//! **Security:** only asymmetric algorithms are accepted (RS256/384/512, ES256).
//! HMAC algorithms are rejected so a token cannot be forged by passing the
//! public key off as an HMAC secret ("alg confusion").

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, info};

use crate::jwt::Claims;

/// A single JSON Web Key (subset we need for signature verification).
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    #[serde(default)]
    pub kid: Option<String>,
    pub kty: String,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    // RSA
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    // EC
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JwksDocument {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
}

/// Configuration for an external IdP verifier.
#[derive(Debug, Clone)]
pub struct ExternalIdpConfig {
    /// Token issuer (`iss`). Required and always enforced.
    pub issuer: String,
    /// Accepted audiences (`aud`). Empty = do not validate audience.
    pub audiences: Vec<String>,
    /// Explicit JWKS URL. If `None`, discovered from the issuer.
    pub jwks_uri: Option<String>,
    /// How long to cache the fetched JWKS before refetching.
    pub jwks_ttl: Duration,
    /// Role assigned when the token carries no recognizable role claim.
    pub default_role: String,
}

impl ExternalIdpConfig {
    pub fn new(issuer: impl Into<String>) -> Self {
        Self {
            issuer: issuer.into(),
            audiences: Vec::new(),
            jwks_uri: None,
            jwks_ttl: Duration::from_secs(3600),
            default_role: "PUBLIC".to_string(),
        }
    }
}

struct CachedJwks {
    keys: Vec<Jwk>,
    fetched_at: Instant,
}

enum JwksSource {
    /// Fetched from the network and cached.
    Remote {
        jwks_uri: Option<String>,
        ttl: Duration,
        cache: Mutex<Option<CachedJwks>>,
    },
    /// Provided directly (tests, or air-gapped deployments that pin keys).
    Static(Vec<Jwk>),
}

/// Verifies externally-issued OAuth 2.x / OIDC JWTs against an IdP's JWKS.
pub struct ExternalIdpVerifier {
    issuer: String,
    audiences: Vec<String>,
    default_role: String,
    allowed_algs: Vec<Algorithm>,
    source: JwksSource,
}

impl ExternalIdpVerifier {
    /// Build a verifier that fetches (and caches) the IdP's JWKS over HTTPS.
    pub fn new(config: ExternalIdpConfig) -> Self {
        Self {
            issuer: config.issuer,
            audiences: config.audiences,
            default_role: config.default_role,
            allowed_algs: default_allowed_algs(),
            source: JwksSource::Remote {
                jwks_uri: config.jwks_uri,
                ttl: config.jwks_ttl,
                cache: Mutex::new(None),
            },
        }
    }

    /// Build a verifier from a fixed set of JWKs (no network). Useful for tests
    /// and deployments that pin their IdP keys.
    pub fn with_static_jwks(
        issuer: impl Into<String>,
        audiences: Vec<String>,
        keys: Vec<Jwk>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            audiences,
            default_role: "PUBLIC".to_string(),
            allowed_algs: default_allowed_algs(),
            source: JwksSource::Static(keys),
        }
    }

    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// Validate a bearer token and map it onto OpenSnow [`Claims`].
    pub async fn verify(&self, token: &str) -> Result<Claims> {
        let header = jsonwebtoken::decode_header(token)
            .context("failed to decode token header")?;

        // Reject anything but the asymmetric algorithms we expect. This is the
        // primary defense against algorithm-confusion attacks.
        if !self.allowed_algs.contains(&header.alg) {
            bail!("token algorithm {:?} is not permitted", header.alg);
        }

        let keys = self.jwks().await?;
        let jwk = select_key(&keys, header.kid.as_deref(), header.alg)
            .ok_or_else(|| anyhow!("no matching JWK for kid={:?}", header.kid))?;
        let decoding_key = decoding_key_for(&header.alg, jwk)?;

        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        if self.audiences.is_empty() {
            validation.validate_aud = false;
        } else {
            let aud: Vec<&str> = self.audiences.iter().map(String::as_str).collect();
            validation.set_audience(&aud);
        }

        let data = jsonwebtoken::decode::<Value>(token, &decoding_key, &validation)
            .context("external token verification failed")?;

        let claims = map_claims(&data.claims, &self.issuer, &self.default_role);
        info!(
            sub = %claims.username,
            role = %claims.role,
            "verified external IdP token"
        );
        Ok(claims)
    }

    /// Return the JWKS, fetching/refreshing from the network when remote.
    async fn jwks(&self) -> Result<Vec<Jwk>> {
        match &self.source {
            JwksSource::Static(keys) => Ok(keys.clone()),
            JwksSource::Remote {
                jwks_uri,
                ttl,
                cache,
            } => {
                if let Some(cached) = cache.lock().expect("jwks cache poisoned").as_ref()
                    && cached.fetched_at.elapsed() < *ttl
                {
                    return Ok(cached.keys.clone());
                }
                let keys = self.fetch_jwks(jwks_uri.as_deref()).await?;
                *cache.lock().expect("jwks cache poisoned") = Some(CachedJwks {
                    keys: keys.clone(),
                    fetched_at: Instant::now(),
                });
                Ok(keys)
            }
        }
    }

    async fn fetch_jwks(&self, configured_uri: Option<&str>) -> Result<Vec<Jwk>> {
        let client = reqwest::Client::new();
        let jwks_uri = match configured_uri {
            Some(uri) => uri.to_string(),
            None => self.discover_jwks_uri(&client).await?,
        };
        debug!(%jwks_uri, "fetching JWKS");
        let doc: JwksDocument = client
            .get(&jwks_uri)
            .send()
            .await
            .context("failed to fetch JWKS")?
            .error_for_status()
            .context("JWKS endpoint returned an error status")?
            .json()
            .await
            .context("failed to parse JWKS")?;
        Ok(doc.keys)
    }

    async fn discover_jwks_uri(&self, client: &reqwest::Client) -> Result<String> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        match client.get(&url).send().await {
            Ok(resp) => match resp.error_for_status() {
                Ok(resp) => match resp.json::<OidcDiscovery>().await {
                    Ok(doc) => return Ok(doc.jwks_uri),
                    Err(e) => debug!("OIDC discovery parse failed: {e}; falling back"),
                },
                Err(e) => debug!("OIDC discovery status error: {e}; falling back"),
            },
            Err(e) => debug!("OIDC discovery request failed: {e}; falling back"),
        }
        // Fallback to the conventional JWKS path.
        Ok(format!(
            "{}/.well-known/jwks.json",
            self.issuer.trim_end_matches('/')
        ))
    }
}

fn default_allowed_algs() -> Vec<Algorithm> {
    vec![
        Algorithm::RS256,
        Algorithm::RS384,
        Algorithm::RS512,
        Algorithm::ES256,
    ]
}

/// Pick the JWK to verify with: prefer an exact `kid` match, otherwise the first
/// signing key whose type matches the token's algorithm family.
fn select_key<'a>(keys: &'a [Jwk], kid: Option<&str>, alg: Algorithm) -> Option<&'a Jwk> {
    if let Some(kid) = kid
        && let Some(k) = keys.iter().find(|k| k.kid.as_deref() == Some(kid))
    {
        return Some(k);
    }
    let want_kty = if is_ec(alg) { "EC" } else { "RSA" };
    keys.iter()
        .find(|k| k.kty == want_kty && k.use_.as_deref() != Some("enc"))
}

fn is_ec(alg: Algorithm) -> bool {
    matches!(alg, Algorithm::ES256 | Algorithm::ES384)
}

fn decoding_key_for(alg: &Algorithm, jwk: &Jwk) -> Result<DecodingKey> {
    if is_ec(*alg) {
        let x = jwk.x.as_deref().ok_or_else(|| anyhow!("EC JWK missing 'x'"))?;
        let y = jwk.y.as_deref().ok_or_else(|| anyhow!("EC JWK missing 'y'"))?;
        DecodingKey::from_ec_components(x, y).context("invalid EC JWK")
    } else {
        let n = jwk.n.as_deref().ok_or_else(|| anyhow!("RSA JWK missing 'n'"))?;
        let e = jwk.e.as_deref().ok_or_else(|| anyhow!("RSA JWK missing 'e'"))?;
        DecodingKey::from_rsa_components(n, e).context("invalid RSA JWK")
    }
}

/// Map verified external JWT claims onto OpenSnow [`Claims`].
fn map_claims(raw: &Value, issuer: &str, default_role: &str) -> Claims {
    let scopes = extract_scopes(raw);
    let role = extract_role(raw).unwrap_or_else(|| default_role.to_string());
    let username = raw
        .get("preferred_username")
        .or_else(|| raw.get("email"))
        .or_else(|| raw.get("sub"))
        .and_then(Value::as_str)
        .unwrap_or("external")
        .to_string();
    let tenant_id = raw
        .get("tenant_id")
        .or_else(|| raw.get("tid"))
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    let exp = raw.get("exp").and_then(Value::as_i64).unwrap_or(0);
    let iat = raw.get("iat").and_then(Value::as_i64).unwrap_or(0);
    let audience = raw
        .get("aud")
        .and_then(|v| v.as_str().map(str::to_string));

    Claims {
        // External identities have no local DB row.
        user_id: 0,
        username,
        role,
        tenant_id,
        scopes,
        session_id: None,
        auth_method: Some("external_idp".to_string()),
        issuer: Some(issuer.to_string()),
        audience,
        iat,
        exp,
    }
}

/// Collect scopes from the common claim shapes: `scope` (space-delimited string,
/// RFC 8693 / OAuth 2), `scp` (string or array, Entra ID), `scopes` (array).
fn extract_scopes(raw: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(s) = raw.get("scope").and_then(Value::as_str) {
        out.extend(s.split_whitespace().map(str::to_string));
    }
    push_string_or_array(raw.get("scp"), &mut out);
    push_string_or_array(raw.get("scopes"), &mut out);
    out.sort();
    out.dedup();
    out
}

/// Derive the primary role from `role` (string) or the first of `roles`/`groups`.
fn extract_role(raw: &Value) -> Option<String> {
    if let Some(r) = raw.get("role").and_then(Value::as_str) {
        return Some(r.to_string());
    }
    for key in ["roles", "groups"] {
        if let Some(first) = raw
            .get(key)
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(Value::as_str)
        {
            return Some(first.to_string());
        }
        if let Some(s) = raw.get(key).and_then(Value::as_str) {
            return Some(s.to_string());
        }
    }
    None
}

fn push_string_or_array(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(s)) => out.extend(s.split_whitespace().map(str::to_string)),
        Some(Value::Array(arr)) => {
            out.extend(arr.iter().filter_map(Value::as_str).map(str::to_string))
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::{EncodingKey, Header};
    use rsa::traits::PublicKeyParts;
    use rsa::{
        RsaPrivateKey,
        pkcs8::{EncodePrivateKey, LineEnding},
    };
    use serde_json::json;

    const KID: &str = "test-key-1";

    struct TestIdp {
        encoding_key: EncodingKey,
        jwk: Jwk,
    }

    fn b64url(bytes: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    fn test_idp() -> TestIdp {
        let mut rng = rand::rngs::OsRng;
        let private_key = RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let public_key = private_key.to_public_key();
        let pem = private_key.to_pkcs8_pem(LineEnding::LF).unwrap();
        let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
        let jwk = Jwk {
            kid: Some(KID.to_string()),
            kty: "RSA".to_string(),
            use_: Some("sig".to_string()),
            alg: Some("RS256".to_string()),
            n: Some(b64url(&public_key.n().to_bytes_be())),
            e: Some(b64url(&public_key.e().to_bytes_be())),
            crv: None,
            x: None,
            y: None,
        };
        TestIdp { encoding_key, jwk }
    }

    fn sign(idp: &TestIdp, claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_string());
        jsonwebtoken::encode(&header, claims, &idp.encoding_key).unwrap()
    }

    fn far_future() -> i64 {
        chrono::Utc::now().timestamp() + 3600
    }

    #[tokio::test]
    async fn verifies_valid_token_and_maps_scopes_and_role() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec!["opensnow-api".to_string()],
            vec![idp.jwk.clone()],
        );
        let token = sign(
            &idp,
            &json!({
                "iss": "https://idp.example",
                "aud": "opensnow-api",
                "sub": "user-123",
                "preferred_username": "alice@corp.example",
                "scope": "sql.query table.select",
                "roles": ["ANALYST"],
                "exp": far_future(),
            }),
        );
        let claims = verifier.verify(&token).await.unwrap();
        assert_eq!(claims.username, "alice@corp.example");
        assert_eq!(claims.role, "ANALYST");
        assert_eq!(claims.scopes, vec!["sql.query", "table.select"]);
        assert_eq!(claims.auth_method.as_deref(), Some("external_idp"));
    }

    #[tokio::test]
    async fn rejects_wrong_issuer() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec![],
            vec![idp.jwk.clone()],
        );
        let token = sign(
            &idp,
            &json!({ "iss": "https://evil.example", "sub": "x", "exp": far_future() }),
        );
        assert!(verifier.verify(&token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_wrong_audience() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec!["opensnow-api".to_string()],
            vec![idp.jwk.clone()],
        );
        let token = sign(
            &idp,
            &json!({ "iss": "https://idp.example", "aud": "some-other-app", "sub": "x", "exp": far_future() }),
        );
        assert!(verifier.verify(&token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_expired_token() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec![],
            vec![idp.jwk.clone()],
        );
        let token = sign(
            &idp,
            // Well past jsonwebtoken's default 60s leeway.
            &json!({ "iss": "https://idp.example", "sub": "x", "exp": chrono::Utc::now().timestamp() - 3600 }),
        );
        assert!(verifier.verify(&token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_token_signed_by_unknown_key() {
        let signer = test_idp();
        let other = test_idp(); // verifier only trusts `other`'s key
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec![],
            vec![other.jwk.clone()],
        );
        let token = sign(
            &signer,
            &json!({ "iss": "https://idp.example", "sub": "x", "exp": far_future() }),
        );
        assert!(verifier.verify(&token).await.is_err());
    }

    #[tokio::test]
    async fn rejects_hmac_algorithm_to_prevent_alg_confusion() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec![],
            vec![idp.jwk.clone()],
        );
        // Forge an HS256 token using the public modulus as the HMAC secret.
        let n_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(idp.jwk.n.as_ref().unwrap())
            .unwrap();
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some(KID.to_string());
        let forged = jsonwebtoken::encode(
            &header,
            &json!({ "iss": "https://idp.example", "sub": "x", "exp": far_future() }),
            &EncodingKey::from_secret(&n_bytes),
        )
        .unwrap();
        assert!(
            verifier.verify(&forged).await.is_err(),
            "HMAC-signed token must be rejected"
        );
    }

    #[tokio::test]
    async fn maps_scp_string_and_default_role() {
        let idp = test_idp();
        let verifier = ExternalIdpVerifier::with_static_jwks(
            "https://idp.example",
            vec![],
            vec![idp.jwk.clone()],
        );
        let token = sign(
            &idp,
            &json!({
                "iss": "https://idp.example",
                "sub": "svc-1",
                "scp": "pipeline.admin",
                "exp": far_future(),
            }),
        );
        let claims = verifier.verify(&token).await.unwrap();
        assert_eq!(claims.scopes, vec!["pipeline.admin"]);
        assert_eq!(claims.role, "PUBLIC"); // default when no role claim
        assert_eq!(claims.username, "svc-1");
    }
}
