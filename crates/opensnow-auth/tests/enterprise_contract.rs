use opensnow_auth::{
    AccountActivation, AuditEventBuilder, AuditResult, AuthDeploymentMode, EnterpriseAuthConfig,
    EntitlementCheck, EntitlementPlan, EntitlementState, ExternalSecretResolver,
    IdentityProviderKind, MarketplaceIdentity, ObjectType, OpenSnowAction, OpenSnowResource,
    Privilege, ScimLifecycleState, SecretHandleDescriptor, SecretProvider, SecretProviderConfig,
    SecretPurpose, SecretState, SecretType, SubjectRef, TrustedSecretStore, WarehouseActivation,
    to_policy_action,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

#[test]
fn maps_sql_table_privilege_into_shared_policy_action() {
    let action = to_policy_action(Privilege::Select, ObjectType::Table).unwrap();
    assert_eq!(action, OpenSnowAction::TableSelect);
    assert_eq!(action.as_str(), "table.select");

    let resource = OpenSnowResource::table("org_acme", "prod", "analytics", "public", "orders");
    let envelope = resource.policy_resource();
    assert_eq!(envelope.resource_type, "table");
    assert_eq!(envelope.resource_id, "prod.analytics.public.orders");
    assert_eq!(envelope.organization_id.as_deref(), Some("org_acme"));
    assert_eq!(envelope.tenant_id.as_deref(), Some("prod"));
}

#[test]
fn models_enterprise_identity_modes_and_provider_requirements() {
    assert!(AuthDeploymentMode::Local.allows_local_credentials());
    assert!(!AuthDeploymentMode::PublicPlatform.allows_local_credentials());
    assert!(AuthDeploymentMode::PublicPlatform.requires_scim());
    assert!(!AuthDeploymentMode::InternalPipeline.requires_marketplace_identity());
    assert!(AuthDeploymentMode::PublicPlatform.requires_marketplace_identity());

    let oidc = IdentityProviderKind::Oidc;
    let saml = IdentityProviderKind::Saml;
    assert_eq!(oidc.as_str(), "oidc");
    assert_eq!(saml.as_str(), "saml");
}

#[test]
fn builds_redacted_audit_event_for_query_without_secret_values() {
    let actor = SubjectRef::user("org_acme", "usr_123", "alice@example.com");
    let resource = OpenSnowResource::query("org_acme", "prod", "qry_456");
    let secret = SecretHandleDescriptor::new(
        "org_acme",
        "sec_s3_stage",
        SecretType::ObjectStorageCredential,
        SecretPurpose::ExternalStage,
    )
    .with_resource_scope("stage://prod.raw.customer_uploads");

    let event = AuditEventBuilder::new(actor, OpenSnowAction::SqlQuery, resource)
        .result(AuditResult::Allowed)
        .trace_id("trace-abc")
        .secret_handle(secret)
        .metadata("sql_hash", "sha256:abc123")
        .build();

    assert_eq!(event.action, "sql.query");
    assert_eq!(event.resource_type, "query");
    assert_eq!(event.result, AuditResult::Allowed);
    assert_eq!(event.secret_handle_refs, vec!["sec_s3_stage"]);
    assert!(event.metadata_redacted.contains_key("sql_hash"));
    assert!(!format!("{event:?}").contains("super-secret"));
}

#[test]
fn captures_scim_and_marketplace_identity_contract_states() {
    assert!(ScimLifecycleState::Active.can_authenticate());
    assert!(!ScimLifecycleState::Deactivated.can_authenticate());

    let listing = MarketplaceIdentity::aws(
        "org_acme",
        "aws-customer-123",
        "prod-abc",
        "entitlement-789",
    );
    assert_eq!(listing.provider.as_str(), "aws");
    assert_eq!(listing.organization_id, "org_acme");
    assert_eq!(listing.external_customer_id, "aws-customer-123");
}

#[test]
fn gates_account_and_warehouse_activation_on_active_entitlements() {
    let marketplace_identity = MarketplaceIdentity::aws(
        "org_acme",
        "aws-customer-123",
        "prod-abc",
        "entitlement-789",
    );
    let entitlement = EntitlementCheck::new(
        marketplace_identity.clone(),
        EntitlementPlan::Enterprise,
        EntitlementState::Active,
    )
    .with_feature("warehouse.activate")
    .with_feature("account.activate")
    .with_warehouse_limit(2);

    assert!(AccountActivation::new("org_acme", "acct_acme").is_allowed(&entitlement));
    assert!(WarehouseActivation::new("org_acme", "prod", "wh_1", 1).is_allowed(&entitlement));
    assert!(!WarehouseActivation::new("org_acme", "prod", "wh_3", 3).is_allowed(&entitlement));

    let expired = EntitlementCheck::new(
        marketplace_identity,
        EntitlementPlan::Enterprise,
        EntitlementState::Expired,
    )
    .with_feature("warehouse.activate")
    .with_feature("account.activate")
    .with_warehouse_limit(2);

    assert!(!AccountActivation::new("org_acme", "acct_acme").is_allowed(&expired));
    assert!(!WarehouseActivation::new("org_acme", "prod", "wh_1", 1).is_allowed(&expired));
}

#[test]
fn sealed_secret_store_never_exposes_raw_values_in_metadata_debug_or_list() {
    let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    let store = TrustedSecretStore::local_dev(conn, "dev-master-key").unwrap();
    let descriptor = SecretHandleDescriptor::new(
        "org_acme",
        "sec_idp_client",
        SecretType::IdpClientSecret,
        SecretPurpose::IdentityProviderClient,
    )
    .with_resource_scope("oidc://acme.okta.example/app/opensnow");

    let created = store
        .create_secret(descriptor, "raw-super-secret-client-value")
        .unwrap();

    assert_eq!(created.handle_id, "sec_idp_client");
    assert_eq!(created.state, SecretState::Active);
    assert!(!format!("{created:?}").contains("raw-super-secret-client-value"));

    let listed = store.list_secrets("org_acme").unwrap();
    assert_eq!(listed.len(), 1);
    let listed_debug = format!("{listed:?}");
    assert!(listed_debug.contains("sec_idp_client"));
    assert!(!listed_debug.contains("raw-super-secret-client-value"));

    let audit = AuditEventBuilder::new(
        SubjectRef::service_identity("org_acme", "svc_auth"),
        OpenSnowAction::IntegrationUse,
        OpenSnowResource::Integration {
            organization_id: "org_acme".to_string(),
            tenant_id: "prod".to_string(),
            name: "okta".to_string(),
        },
    )
    .secret_handle(created.descriptor.clone())
    .build();
    assert_eq!(audit.secret_handle_refs, vec!["sec_idp_client"]);
    assert!(
        !serde_json::to_string(&audit)
            .unwrap()
            .contains("raw-super-secret-client-value")
    );
}

#[test]
fn sealed_secret_resolution_rotation_and_revoke_are_trusted_path_only() {
    let conn = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
    let store = TrustedSecretStore::local_dev(conn, "dev-master-key").unwrap();
    let descriptor = SecretHandleDescriptor::new(
        "org_acme",
        "sec_stage_s3",
        SecretType::ObjectStorageCredential,
        SecretPurpose::ExternalStage,
    );

    store.create_secret(descriptor, "old-secret-value").unwrap();
    let resolved = store.resolve_secret("org_acme", "sec_stage_s3").unwrap();
    assert_eq!(
        resolved.expose_to_trusted_execution_path(),
        "old-secret-value"
    );
    assert!(!format!("{resolved:?}").contains("old-secret-value"));

    let rotated = store
        .rotate_secret("org_acme", "sec_stage_s3", "new-secret-value")
        .unwrap();
    assert_eq!(rotated.version, 2);
    assert_eq!(
        store
            .resolve_secret("org_acme", "sec_stage_s3")
            .unwrap()
            .expose_to_trusted_execution_path(),
        "new-secret-value"
    );

    let revoked = store.revoke_secret("org_acme", "sec_stage_s3").unwrap();
    assert_eq!(revoked.state, SecretState::Revoked);
    assert!(store.resolve_secret("org_acme", "sec_stage_s3").is_err());
}

#[test]
fn provider_config_models_cloud_and_vault_handles_without_plaintext() {
    let aws = SecretProviderConfig::aws_secrets_manager(
        "arn:aws:secretsmanager:eu-north-1:123456789012:secret:opensnow/prod/oidc",
        Some("arn:aws:kms:eu-north-1:123456789012:key/abcd"),
    );
    let gcp = SecretProviderConfig::gcp_secret_manager(
        "projects/acme/secrets/opensnow-prod-oidc",
        Some("projects/acme/locations/global/keyRings/opensnow/cryptoKeys/prod"),
    );
    let vault =
        SecretProviderConfig::vault("kv/data/opensnow/prod/oidc", Some("transit/keys/opensnow"));

    for provider in [aws, gcp, vault] {
        let serialized = serde_json::to_string(&provider).unwrap();
        assert!(serialized.contains("handle_ref") || serialized.contains("path"));
        assert!(!serialized.contains("client-secret-value"));
        assert!(provider.is_enterprise_backed());
    }
}

#[test]
fn enterprise_auth_config_fails_closed_on_plaintext_or_missing_secret_boundary() {
    let local_demo = EnterpriseAuthConfig::local_demo();
    assert!(local_demo.validate().is_ok());

    let mut bad = EnterpriseAuthConfig::enterprise();
    bad.jwt_issuer = None;
    bad.jwt_audience = None;
    bad.jwks_url = None;
    bad.secret_provider = SecretProviderConfig::local_dev("demo-key");
    bad.allow_plaintext_object_store_keys = true;
    bad.default_admin_password = Some("opensnow".to_string());

    let err = bad.validate().unwrap_err().to_string();
    assert!(err.contains("jwt issuer"));
    assert!(err.contains("jwt audience"));
    assert!(err.contains("JWKS"));
    assert!(err.contains("enterprise-backed secret provider"));
    assert!(err.contains("plaintext object-store keys"));
    assert!(err.contains("default admin password"));
}

#[test]
fn external_secret_handles_parse_to_production_resolvers_without_plaintext() {
    let aws = ExternalSecretResolver::from_handle(
        "aws-secretsmanager://arn:aws:secretsmanager:eu-north-1:123456789012:secret:opensnow/prod/oidc",
    )
    .unwrap();
    assert_eq!(aws.provider_name(), "aws-secrets-manager");
    assert!(format!("{aws:?}").contains("aws-secrets-manager"));
    assert!(!format!("{aws:?}").contains("client-secret-value"));

    let vault =
        ExternalSecretResolver::from_handle("vault://kv/data/opensnow/prod/oidc#client_secret")
            .unwrap();
    assert_eq!(vault.provider_name(), "vault");

    let unsupported = ExternalSecretResolver::from_handle("local-dev://raw-secret");
    assert!(
        unsupported
            .unwrap_err()
            .to_string()
            .contains("unsupported external secret handle")
    );
}

#[test]
fn configured_external_secret_provider_fails_closed_when_runtime_dependency_is_missing() {
    let resolver = ExternalSecretResolver::from_handle("aws-secretsmanager://opensnow/prod/oidc")
        .unwrap()
        .with_command_override("/definitely/missing/opensnow/aws");

    let err = resolver.resolve().unwrap_err().to_string();
    assert!(err.contains("failed closed"));
    assert!(!err.contains("opensnow/prod/oidc client secret"));
}
