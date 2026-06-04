use serde::Deserialize;
use std::path::Path;

use crate::engine::EngineConfig;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OpenSnowConfig {
    pub server: ServerConfig,
    pub storage: EngineConfig,
    pub catalog: CatalogConfig,
    pub enterprise: EnterpriseConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub http_port: u16,
    pub pg_port: u16,
    pub pg_enabled: bool,
    pub host: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CatalogConfig {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EnterpriseConfig {
    pub mode: String,
    pub entitlement_required: bool,
    pub marketplace_provider: String,
    pub secret_provider: EnterpriseSecretProviderConfig,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EnterpriseSecretProviderConfig {
    pub enabled: bool,
    pub provider: String,
    pub kms_key_arn: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_port: 8080,
            pg_port: 5433,
            pg_enabled: false,
            // Safe by default: bind to loopback only. Set [server].host (or
            // OPENSNOW_SERVER_HOST) to "0.0.0.0" to expose the listeners, which
            // requires authentication or an explicit OPENSNOW_ALLOW_PUBLIC=1.
            host: "127.0.0.1".to_string(),
        }
    }
}

impl Default for CatalogConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Self {
            path: format!("{home}/.opensnow/catalog.db"),
        }
    }
}

impl Default for EnterpriseConfig {
    fn default() -> Self {
        Self {
            mode: "test-instance".to_string(),
            entitlement_required: false,
            marketplace_provider: String::new(),
            secret_provider: EnterpriseSecretProviderConfig::default(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn server_config_disables_pgwire_by_default_for_public_demo_safety() {
        let config = OpenSnowConfig::default();

        assert!(!config.server.pg_enabled);
    }

    #[test]
    fn server_config_can_opt_in_to_pgwire_explicitly() {
        let config: OpenSnowConfig = toml::from_str(
            r#"
            [server]
            pg_enabled = true
            pg_port = 55433
            "#,
        )
        .expect("config should parse pgwire opt-in");

        assert!(config.server.pg_enabled);
        assert_eq!(config.server.pg_port, 55433);
    }

    #[test]
    fn enterprise_mode_config_requires_external_secret_provider_runtime_enforcement() {
        let config: OpenSnowConfig = toml::from_str(
            r#"
            [enterprise]
            mode = "aws-marketplace"
            entitlement_required = true
            marketplace_provider = "aws"

            [enterprise.secret_provider]
            enabled = true
            provider = "aws-secrets-manager"
            kms_key_arn = "arn:aws:kms:eu-north-1:123456789012:key/abcd"
            "#,
        )
        .expect("enterprise config should parse");

        config
            .validate_runtime_deployment()
            .expect("AWS marketplace secret-provider config should pass runtime enforcement");
    }

    #[test]
    fn enterprise_mode_config_rejects_local_secret_provider_and_plaintext_storage() {
        let config: OpenSnowConfig = toml::from_str(
            r#"
            [storage]
            s3_access_key = "AKIAINLINE"
            s3_secret_key = "inline-secret"

            [enterprise]
            mode = "enterprise"
            entitlement_required = true
            marketplace_provider = "aws"

            [enterprise.secret_provider]
            enabled = false
            provider = "local-dev"
            kms_key_arn = ""
            "#,
        )
        .expect("enterprise config should parse");

        let err = config
            .validate_runtime_deployment()
            .unwrap_err()
            .to_string();
        assert!(err.contains("enterprise.secret_provider.enabled=true"));
        assert!(err.contains("aws-secrets-manager, gcp-secret-manager, or vault"));
        assert!(err.contains("inline object-store credentials"));
    }

    #[test]
    fn object_storage_credentials_can_be_injected_from_env_without_config_file_secrets() {
        let _guard = ENV_LOCK.lock().expect("env lock should not be poisoned");
        let path = std::env::temp_dir().join(format!(
            "opensnow-config-env-test-{}.toml",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"
            [storage]
            s3_bucket = "from-file"
            s3_region = "eu-north-1"
            "#,
        )
        .expect("write test config");

        unsafe {
            std::env::set_var("OPENSNOW_STORAGE_ACCESS_KEY", "OPEN/SNOW/DEMO/ONLY");
            std::env::set_var("OPENSNOW_STORAGE_SECRET_KEY", "synthetic-demo-secret");
            std::env::set_var("OPENSNOW_STORAGE_S3_BUCKET", "from-env");
            std::env::set_var("OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP", "true");
        }

        let config = OpenSnowConfig::load_from(path.to_str().expect("utf-8 path"))
            .expect("config should load");

        assert_eq!(config.storage.s3_bucket.as_deref(), Some("from-env"));
        assert_eq!(
            config.storage.s3_access_key.as_deref(),
            Some("OPEN/SNOW/DEMO/ONLY")
        );
        assert_eq!(
            config.storage.s3_secret_key.as_deref(),
            Some("synthetic-demo-secret")
        );
        assert_eq!(config.storage.s3_region.as_deref(), Some("eu-north-1"));
        assert!(config.storage.s3_allow_insecure_http);

        unsafe {
            std::env::remove_var("OPENSNOW_STORAGE_ACCESS_KEY");
            std::env::remove_var("OPENSNOW_STORAGE_SECRET_KEY");
            std::env::remove_var("OPENSNOW_STORAGE_S3_BUCKET");
            std::env::remove_var("OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP");
        }
        let _ = std::fs::remove_file(path);
    }
}

impl OpenSnowConfig {
    pub fn validate_runtime_deployment(&self) -> anyhow::Result<()> {
        let mode = self.enterprise.mode.trim();
        if mode.is_empty() || mode == "test-instance" || mode == "local" {
            return Ok(());
        }

        let mut errors = Vec::new();
        let provider = self.enterprise.secret_provider.provider.trim();
        if !self.enterprise.secret_provider.enabled {
            errors.push("enterprise.secret_provider.enabled=true is required".to_string());
        }
        if !matches!(
            provider,
            "aws-secrets-manager" | "gcp-secret-manager" | "vault"
        ) {
            errors.push(
                "enterprise.secret_provider.provider must be aws-secrets-manager, gcp-secret-manager, or vault"
                    .to_string(),
            );
        }
        if self
            .enterprise
            .secret_provider
            .kms_key_arn
            .trim()
            .is_empty()
            && provider != "vault"
        {
            errors.push(
                "enterprise.secret_provider.kms_key_arn is required for cloud KMS-backed providers"
                    .to_string(),
            );
        }
        if self.storage.s3_access_key.is_some()
            || self.storage.s3_secret_key.is_some()
            || self.storage.gcs_service_account_path.is_some()
            || self.storage.azure_account_key.is_some()
            || self.storage.azure_client_secret.is_some()
        {
            errors.push(
                "enterprise runtime forbids inline object-store credentials; use workload identity or secret handles"
                    .to_string(),
            );
        }
        if (mode == "aws-marketplace" || self.enterprise.entitlement_required)
            && self.enterprise.marketplace_provider.trim().is_empty()
        {
            errors.push(
                "enterprise marketplace_provider is required when entitlements are enforced"
                    .to_string(),
            );
        }

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(errors.join("; "))
        }
    }

    /// Load config from file, falling back to defaults.
    /// Checks: ./opensnow.toml, ~/.config/opensnow/config.toml
    pub fn load() -> Self {
        let candidates = vec![
            "opensnow.toml".to_string(),
            format!(
                "{}/.config/opensnow/config.toml",
                std::env::var("HOME").unwrap_or_default()
            ),
        ];

        for path in &candidates {
            if !Path::new(path).exists() {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(path) {
                match toml::from_str::<OpenSnowConfig>(&content) {
                    Ok(mut config) => {
                        config.expand_tildes();
                        config.apply_env_overrides();
                        if let Err(err) = config.validate_runtime_deployment() {
                            panic!("invalid OpenSnow deployment config in {path}: {err}");
                        }
                        tracing::info!("Loaded config from {}", path);
                        return config;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse {}: {}", path, e);
                    }
                }
            }
        }

        tracing::info!("No config file found, using defaults");
        Self::default()
    }

    /// Load from a specific file path.
    pub fn load_from(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let mut config: OpenSnowConfig = toml::from_str(&content)?;
        config.expand_tildes();
        config.apply_env_overrides();
        config.validate_runtime_deployment()?;
        Ok(config)
    }

    /// Replace ~ with $HOME in all path fields.
    fn expand_tildes(&mut self) {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        self.storage.warehouse_path = self.storage.warehouse_path.replace("~", &home);
        self.catalog.path = self.catalog.path.replace("~", &home);
    }

    /// Apply deployment-safe environment overrides after parsing TOML.
    ///
    /// Helm/Kubernetes inject object-storage credentials as process env vars from
    /// Secrets so secrets never have to be rendered into the mounted ConfigMap.
    fn apply_env_overrides(&mut self) {
        if let Ok(value) = std::env::var("OPENSNOW_SERVER_HOST")
            && let Some(host) = non_empty(value)
        {
            self.server.host = host;
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_ACCESS_KEY") {
            self.storage.s3_access_key = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_SECRET_KEY") {
            self.storage.s3_secret_key = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_S3_BUCKET") {
            self.storage.s3_bucket = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_S3_REGION") {
            self.storage.s3_region = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_S3_ENDPOINT") {
            self.storage.s3_endpoint = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP") {
            self.storage.s3_allow_insecure_http = parse_bool(&value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_GCS_BUCKET") {
            self.storage.gcs_bucket = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_GCS_PROJECT_ID") {
            self.storage.gcs_project_id = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_GCS_SERVICE_ACCOUNT_PATH") {
            self.storage.gcs_service_account_path = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_CONTAINER") {
            self.storage.azure_container = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_ACCOUNT_NAME") {
            self.storage.azure_account_name = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_ACCOUNT_KEY") {
            self.storage.azure_account_key = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_CLIENT_ID") {
            self.storage.azure_client_id = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_CLIENT_SECRET") {
            self.storage.azure_client_secret = non_empty(value);
        }
        if let Ok(value) = std::env::var("OPENSNOW_STORAGE_AZURE_TENANT_ID") {
            self.storage.azure_tenant_id = non_empty(value);
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_bool(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}
