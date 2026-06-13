pub mod contract;
pub mod jwt;
pub mod middleware;
pub mod oidc_verifier;
pub mod privileges;
pub mod roles;
pub mod secrets;
pub mod sso;
pub mod users;

pub use contract::{
    AccountActivation, AuditEvent, AuditEventBuilder, AuditResult, AuthDeploymentMode,
    EnterpriseAuthConfig, EntitlementCheck, EntitlementPlan, EntitlementState,
    IdentityProviderKind, MarketplaceIdentity, MarketplaceProvider, OpenSnowAction,
    OpenSnowResource, PolicyResource, ScimLifecycleState, SecretHandleDescriptor, SecretPurpose,
    SecretType, SubjectRef, WarehouseActivation, to_policy_action,
};
pub use jwt::{
    Claims, EnterpriseJwtConfig, EnterpriseJwtKey, JsonWebKey, JwtManager, SsoSessionTokenRequest,
};
pub use middleware::AuthConfig;
pub use oidc_verifier::{ExternalIdpConfig, ExternalIdpVerifier, Jwk};
pub use privileges::{ObjectType, Privilege, PrivilegeStore};
pub use roles::{BuiltinRole, RoleStore};
pub use secrets::{
    ExternalSecretResolver, SecretMetadata, SecretProvider, SecretProviderConfig, SecretState,
    SecretValue, TrustedSecretStore,
};
pub use sso::{
    IdpConnection, IdpConnectionUpsert, OidcLoginStart, RedactedIdpConnection,
    RedactedIdpRoleMapping, SSO_SCHEMA_SQL, SsoLoginRequest, SsoManager, SsoProtocol, SsoSession,
    TenantSsoConfig, VerifiedOidcClaims,
};
pub use users::{User, UserStore};
