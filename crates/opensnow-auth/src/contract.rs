//! Enterprise auth/compliance contract adapters for OpenSnow.
//!
//! These product-local types give the server, pgwire, catalog, and future
//! SCIM/marketplace layers one stable vocabulary for policy decisions, audit
//! events, sealed secret handles, and deployment-mode requirements.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::privileges::{ObjectType, Privilege};
use crate::secrets::SecretProviderConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthDeploymentMode {
    Local,
    InternalPipeline,
    PublicPlatform,
}

impl AuthDeploymentMode {
    pub fn allows_local_credentials(self) -> bool {
        matches!(self, Self::Local | Self::InternalPipeline)
    }

    pub fn requires_scim(self) -> bool {
        matches!(self, Self::PublicPlatform)
    }

    pub fn requires_marketplace_identity(self) -> bool {
        matches!(self, Self::PublicPlatform)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityProviderKind {
    Local,
    Oidc,
    Saml,
    ServiceAccount,
}

impl IdentityProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Oidc => "oidc",
            Self::Saml => "saml",
            Self::ServiceAccount => "service_account",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScimLifecycleState {
    Active,
    Suspended,
    Deactivated,
}

impl ScimLifecycleState {
    pub fn can_authenticate(self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectRef {
    pub organization_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub display: Option<String>,
    pub auth_method: Option<String>,
}

impl SubjectRef {
    pub fn user(
        organization_id: impl Into<String>,
        subject_id: impl Into<String>,
        display: impl Into<String>,
    ) -> Self {
        Self {
            organization_id: organization_id.into(),
            subject_type: "user".to_string(),
            subject_id: subject_id.into(),
            display: Some(display.into()),
            auth_method: None,
        }
    }

    pub fn service_identity(
        organization_id: impl Into<String>,
        subject_id: impl Into<String>,
    ) -> Self {
        Self {
            organization_id: organization_id.into(),
            subject_type: "service_identity".to_string(),
            subject_id: subject_id.into(),
            display: None,
            auth_method: Some(IdentityProviderKind::ServiceAccount.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenSnowAction {
    SqlQuery,
    WarehouseUse,
    WarehouseResize,
    DatabaseCreate,
    SchemaUse,
    TableSelect,
    TableInsert,
    StageRead,
    StageWrite,
    IntegrationUse,
    PolicyAdmin,
    AuditRead,
}

impl OpenSnowAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SqlQuery => "sql.query",
            Self::WarehouseUse => "warehouse.use",
            Self::WarehouseResize => "warehouse.resize",
            Self::DatabaseCreate => "database.create",
            Self::SchemaUse => "schema.use",
            Self::TableSelect => "table.select",
            Self::TableInsert => "table.insert",
            Self::StageRead => "stage.read",
            Self::StageWrite => "stage.write",
            Self::IntegrationUse => "integration.use",
            Self::PolicyAdmin => "policy.admin",
            Self::AuditRead => "audit.read",
        }
    }
}

pub fn to_policy_action(privilege: Privilege, object_type: ObjectType) -> Option<OpenSnowAction> {
    match (privilege, object_type) {
        (Privilege::Select, ObjectType::Table) => Some(OpenSnowAction::TableSelect),
        (Privilege::Insert, ObjectType::Table) => Some(OpenSnowAction::TableInsert),
        (Privilege::Create, ObjectType::Database) => Some(OpenSnowAction::DatabaseCreate),
        (Privilege::Create, ObjectType::Schema) => Some(OpenSnowAction::SchemaUse),
        (Privilege::Alter | Privilege::Drop, _) => Some(OpenSnowAction::PolicyAdmin),
        (Privilege::All, ObjectType::Table) => Some(OpenSnowAction::TableSelect),
        (Privilege::All, ObjectType::Database) => Some(OpenSnowAction::DatabaseCreate),
        (Privilege::All, ObjectType::Schema) => Some(OpenSnowAction::SchemaUse),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyResource {
    pub organization_id: Option<String>,
    pub tenant_id: Option<String>,
    pub resource_type: String,
    pub resource_id: String,
    pub resource_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenSnowResource {
    Warehouse {
        organization_id: String,
        tenant_id: String,
        name: String,
    },
    Database {
        organization_id: String,
        tenant_id: String,
        database: String,
    },
    Schema {
        organization_id: String,
        tenant_id: String,
        database: String,
        schema: String,
    },
    Table {
        organization_id: String,
        tenant_id: String,
        database: String,
        schema: String,
        table: String,
    },
    Stage {
        organization_id: String,
        tenant_id: String,
        stage: String,
    },
    Integration {
        organization_id: String,
        tenant_id: String,
        name: String,
    },
    Query {
        organization_id: String,
        tenant_id: String,
        query_id: String,
    },
}

impl OpenSnowResource {
    pub fn table(
        organization_id: impl Into<String>,
        tenant_id: impl Into<String>,
        database: impl Into<String>,
        schema: impl Into<String>,
        table: impl Into<String>,
    ) -> Self {
        Self::Table {
            organization_id: organization_id.into(),
            tenant_id: tenant_id.into(),
            database: database.into(),
            schema: schema.into(),
            table: table.into(),
        }
    }

    pub fn query(
        organization_id: impl Into<String>,
        tenant_id: impl Into<String>,
        query_id: impl Into<String>,
    ) -> Self {
        Self::Query {
            organization_id: organization_id.into(),
            tenant_id: tenant_id.into(),
            query_id: query_id.into(),
        }
    }

    pub fn policy_resource(&self) -> PolicyResource {
        match self {
            Self::Warehouse {
                organization_id,
                tenant_id,
                name,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "warehouse".to_string(),
                resource_id: format!("{tenant_id}.{name}"),
                resource_name: Some(name.clone()),
            },
            Self::Database {
                organization_id,
                tenant_id,
                database,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "database".to_string(),
                resource_id: format!("{tenant_id}.{database}"),
                resource_name: Some(database.clone()),
            },
            Self::Schema {
                organization_id,
                tenant_id,
                database,
                schema,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "schema".to_string(),
                resource_id: format!("{tenant_id}.{database}.{schema}"),
                resource_name: Some(format!("{database}.{schema}")),
            },
            Self::Table {
                organization_id,
                tenant_id,
                database,
                schema,
                table,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "table".to_string(),
                resource_id: format!("{tenant_id}.{database}.{schema}.{table}"),
                resource_name: Some(format!("{database}.{schema}.{table}")),
            },
            Self::Stage {
                organization_id,
                tenant_id,
                stage,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "stage".to_string(),
                resource_id: format!("{tenant_id}.{stage}"),
                resource_name: Some(stage.clone()),
            },
            Self::Integration {
                organization_id,
                tenant_id,
                name,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "integration".to_string(),
                resource_id: format!("{tenant_id}.{name}"),
                resource_name: Some(name.clone()),
            },
            Self::Query {
                organization_id,
                tenant_id,
                query_id,
            } => PolicyResource {
                organization_id: Some(organization_id.clone()),
                tenant_id: Some(tenant_id.clone()),
                resource_type: "query".to_string(),
                resource_id: query_id.clone(),
                resource_name: Some(query_id.clone()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditResult {
    Allowed,
    Denied,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretType {
    ObjectStorageCredential,
    ExternalStageCredential,
    CatalogIntegrationCredential,
    BiOAuthClientCredential,
    IdpClientSecret,
    EncryptionKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretPurpose {
    ObjectStorageAccess,
    ExternalStage,
    CatalogIntegration,
    BiClientAuth,
    IdentityProviderClient,
    Encryption,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretHandleDescriptor {
    pub organization_id: String,
    pub handle_id: String,
    pub secret_type: SecretType,
    pub purpose: SecretPurpose,
    pub resource_scope: Option<String>,
    pub reusable: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

impl SecretHandleDescriptor {
    pub fn new(
        organization_id: impl Into<String>,
        handle_id: impl Into<String>,
        secret_type: SecretType,
        purpose: SecretPurpose,
    ) -> Self {
        Self {
            organization_id: organization_id.into(),
            handle_id: handle_id.into(),
            secret_type,
            purpose,
            resource_scope: None,
            reusable: true,
            expires_at: None,
        }
    }

    pub fn with_resource_scope(mut self, resource_scope: impl Into<String>) -> Self {
        self.resource_scope = Some(resource_scope.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub event_time: DateTime<Utc>,
    pub organization_id: String,
    pub tenant_id: Option<String>,
    pub actor_type: String,
    pub actor_id: String,
    pub actor_display: Option<String>,
    pub actor_auth_method: Option<String>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resource_name: Option<String>,
    pub result: AuditResult,
    pub trace_id: Option<String>,
    pub secret_handle_refs: Vec<String>,
    pub metadata_redacted: Map<String, Value>,
}

pub struct AuditEventBuilder {
    actor: SubjectRef,
    action: OpenSnowAction,
    resource: OpenSnowResource,
    result: AuditResult,
    trace_id: Option<String>,
    secret_handle_refs: Vec<String>,
    metadata_redacted: Map<String, Value>,
}

impl AuditEventBuilder {
    pub fn new(actor: SubjectRef, action: OpenSnowAction, resource: OpenSnowResource) -> Self {
        Self {
            actor,
            action,
            resource,
            result: AuditResult::Succeeded,
            trace_id: None,
            secret_handle_refs: Vec::new(),
            metadata_redacted: Map::new(),
        }
    }

    pub fn result(mut self, result: AuditResult) -> Self {
        self.result = result;
        self
    }

    pub fn trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    pub fn secret_handle(mut self, secret: SecretHandleDescriptor) -> Self {
        self.secret_handle_refs.push(secret.handle_id);
        self
    }

    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.metadata_redacted.insert(key.into(), value.into());
        self
    }

    pub fn build(self) -> AuditEvent {
        let policy_resource = self.resource.policy_resource();
        AuditEvent {
            event_time: Utc::now(),
            organization_id: self.actor.organization_id,
            tenant_id: policy_resource.tenant_id,
            actor_type: self.actor.subject_type,
            actor_id: self.actor.subject_id,
            actor_display: self.actor.display,
            actor_auth_method: self.actor.auth_method,
            action: self.action.as_str().to_string(),
            resource_type: policy_resource.resource_type,
            resource_id: policy_resource.resource_id,
            resource_name: policy_resource.resource_name,
            result: self.result,
            trace_id: self.trace_id,
            secret_handle_refs: self.secret_handle_refs,
            metadata_redacted: self.metadata_redacted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketplaceProvider {
    Aws,
    Gcp,
    Azure,
}

impl MarketplaceProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Aws => "aws",
            Self::Gcp => "gcp",
            Self::Azure => "azure",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketplaceIdentity {
    pub organization_id: String,
    pub provider: MarketplaceProvider,
    pub external_customer_id: String,
    pub product_code: String,
    pub entitlement_id: String,
}

impl MarketplaceIdentity {
    pub fn aws(
        organization_id: impl Into<String>,
        external_customer_id: impl Into<String>,
        product_code: impl Into<String>,
        entitlement_id: impl Into<String>,
    ) -> Self {
        Self {
            organization_id: organization_id.into(),
            provider: MarketplaceProvider::Aws,
            external_customer_id: external_customer_id.into(),
            product_code: product_code.into(),
            entitlement_id: entitlement_id.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntitlementPlan {
    Free,
    Pro,
    Enterprise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntitlementState {
    Active,
    Trialing,
    Expired,
    Suspended,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntitlementCheck {
    pub marketplace_identity: MarketplaceIdentity,
    pub plan: EntitlementPlan,
    pub state: EntitlementState,
    pub features: Vec<String>,
    pub warehouse_limit: Option<usize>,
}

impl EntitlementCheck {
    pub fn new(
        marketplace_identity: MarketplaceIdentity,
        plan: EntitlementPlan,
        state: EntitlementState,
    ) -> Self {
        Self {
            marketplace_identity,
            plan,
            state,
            features: Vec::new(),
            warehouse_limit: None,
        }
    }

    pub fn with_feature(mut self, feature: impl Into<String>) -> Self {
        self.features.push(feature.into());
        self
    }

    pub fn with_warehouse_limit(mut self, limit: usize) -> Self {
        self.warehouse_limit = Some(limit);
        self
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            EntitlementState::Active | EntitlementState::Trialing
        )
    }

    pub fn has_feature(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountActivation {
    pub organization_id: String,
    pub account_id: String,
}

impl AccountActivation {
    pub fn new(organization_id: impl Into<String>, account_id: impl Into<String>) -> Self {
        Self {
            organization_id: organization_id.into(),
            account_id: account_id.into(),
        }
    }

    pub fn is_allowed(&self, entitlement: &EntitlementCheck) -> bool {
        entitlement.is_active()
            && entitlement.has_feature("account.activate")
            && entitlement.marketplace_identity.organization_id == self.organization_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarehouseActivation {
    pub organization_id: String,
    pub tenant_id: String,
    pub warehouse_id: String,
    pub requested_warehouse_count: usize,
}

impl WarehouseActivation {
    pub fn new(
        organization_id: impl Into<String>,
        tenant_id: impl Into<String>,
        warehouse_id: impl Into<String>,
        requested_warehouse_count: usize,
    ) -> Self {
        Self {
            organization_id: organization_id.into(),
            tenant_id: tenant_id.into(),
            warehouse_id: warehouse_id.into(),
            requested_warehouse_count,
        }
    }

    pub fn is_allowed(&self, entitlement: &EntitlementCheck) -> bool {
        entitlement.is_active()
            && entitlement.has_feature("warehouse.activate")
            && entitlement.marketplace_identity.organization_id == self.organization_id
            && entitlement
                .warehouse_limit
                .map(|limit| self.requested_warehouse_count <= limit)
                .unwrap_or(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnterpriseAuthConfig {
    pub deployment_mode: AuthDeploymentMode,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
    pub jwks_url: Option<String>,
    pub secret_provider: SecretProviderConfig,
    pub allow_plaintext_object_store_keys: bool,
    pub default_admin_password: Option<String>,
}

impl EnterpriseAuthConfig {
    pub fn local_demo() -> Self {
        Self {
            deployment_mode: AuthDeploymentMode::Local,
            jwt_issuer: None,
            jwt_audience: None,
            jwks_url: None,
            secret_provider: SecretProviderConfig::local_dev("local-dev"),
            allow_plaintext_object_store_keys: true,
            default_admin_password: Some("opensnow".to_string()),
        }
    }

    pub fn enterprise() -> Self {
        Self {
            deployment_mode: AuthDeploymentMode::PublicPlatform,
            jwt_issuer: Some("https://auth.opensnow.example".to_string()),
            jwt_audience: Some("opensnow".to_string()),
            jwks_url: Some("https://auth.opensnow.example/.well-known/jwks.json".to_string()),
            secret_provider: SecretProviderConfig::vault(
                "kv/data/opensnow/prod",
                Some("transit/keys/opensnow"),
            ),
            allow_plaintext_object_store_keys: false,
            default_admin_password: None,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.deployment_mode == AuthDeploymentMode::Local {
            return Ok(());
        }
        let mut errors = Vec::new();
        if self.jwt_issuer.as_deref().unwrap_or_default().is_empty() {
            errors.push("jwt issuer is required");
        }
        if self.jwt_audience.as_deref().unwrap_or_default().is_empty() {
            errors.push("jwt audience is required");
        }
        if self.jwks_url.as_deref().unwrap_or_default().is_empty() {
            errors.push("JWKS URL is required");
        }
        if !self.secret_provider.is_enterprise_backed() {
            errors.push("enterprise-backed secret provider is required");
        }
        if self.allow_plaintext_object_store_keys {
            errors.push("plaintext object-store keys are forbidden");
        }
        if self.default_admin_password.is_some() {
            errors.push("default admin password is forbidden");
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(errors.join("; "))
        }
    }
}
