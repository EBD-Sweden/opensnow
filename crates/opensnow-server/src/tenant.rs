//! Tenant extraction & request scoping.
//!
//! The HTTP layer reads `X-Tenant-ID` off each request; if absent the request
//! is bound to the [`DEFAULT_TENANT`]. The resolved id is attached to the
//! request as an extension so handlers downstream can read it via
//! [`TenantId`].

use axum::{
    body::Body,
    extract::{FromRequestParts, Request},
    http::{HeaderMap, StatusCode, request::Parts},
    middleware::Next,
    response::Response,
};
use opensnow_catalog::DEFAULT_TENANT;

pub const TENANT_HEADER: &str = "X-Tenant-ID";

/// Resolved tenant id for the current request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TenantId {
    fn default() -> Self {
        TenantId(DEFAULT_TENANT.to_string())
    }
}

fn parse_header(headers: &HeaderMap) -> Option<TenantId> {
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| TenantId(s.to_string()))
}

/// Axum middleware that resolves the tenant id from the `X-Tenant-ID` header
/// and stores it as a request extension.
pub async fn tenant_middleware(mut req: Request<Body>, next: Next) -> Response {
    let tenant = parse_header(req.headers()).unwrap_or_default();
    req.extensions_mut().insert(tenant);
    next.run(req).await
}

impl<S> FromRequestParts<S> for TenantId
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, &'static str);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // First try the extension installed by the middleware. If unset (e.g.
        // tests that hit a handler directly) fall back to a header parse.
        if let Some(t) = parts.extensions.get::<TenantId>() {
            return Ok(t.clone());
        }
        Ok(parse_header(&parts.headers).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn header_present() {
        let mut h = HeaderMap::new();
        h.insert(TENANT_HEADER, HeaderValue::from_static("acme"));
        assert_eq!(parse_header(&h), Some(TenantId("acme".to_string())));
    }

    #[test]
    fn header_missing_falls_back_to_default() {
        let h = HeaderMap::new();
        assert_eq!(parse_header(&h), None);
        assert_eq!(TenantId::default().as_str(), "default");
    }

    #[test]
    fn header_blank_treated_as_missing() {
        let mut h = HeaderMap::new();
        h.insert(TENANT_HEADER, HeaderValue::from_static("   "));
        assert_eq!(parse_header(&h), None);
    }
}
