use serde::Serialize;

use crate::OpenSnowConfig;

/// Product-specific command-line contract for OpenSnow.
///
/// This module intentionally describes OpenSnow command-line input surfaces only
/// and exposes its own stable, self-contained contract vocabulary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenSnowCliReport {
    pub product: String,
    pub lane: String,
    pub target: String,
    pub status: CliOverallStatus,
    pub commands: Vec<CliCommandSpec>,
    pub config: CliConfigSpec,
    pub schemas: Vec<String>,
    pub agent_contract: CliAgentContract,
    pub checks: Vec<CliReadinessCheck>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliOverallStatus {
    Ready,
    NeedsProductionConfig,
    Blocked,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CliCommandSpec {
    pub name: String,
    pub purpose: String,
    pub required_inputs: Vec<String>,
    pub optional_inputs: Vec<String>,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CliConfigSpec {
    pub default_file: String,
    pub global_flags: Vec<String>,
    pub environment: Vec<String>,
    pub secret_policy: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CliAgentContract {
    pub version: String,
    pub required_scopes: Vec<String>,
    pub stable_commands: Vec<String>,
    pub api_endpoints: Vec<String>,
    pub output_formats: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CliReadinessCheck {
    pub id: String,
    pub title: String,
    pub status: CliCheckStatus,
    pub evidence: String,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CliCheckStatus {
    Pass,
    Warn,
    Fail,
}

impl OpenSnowCliReport {
    pub fn from_config(config: &OpenSnowConfig) -> Self {
        let checks = vec![
            config_file_check(config),
            storage_check(config),
            catalog_check(config),
            enterprise_secret_provider_check(config),
            entitlement_check(config),
            pgwire_policy_check(config),
        ];
        let status = if checks
            .iter()
            .any(|check| check.status == CliCheckStatus::Fail)
        {
            CliOverallStatus::Blocked
        } else if checks
            .iter()
            .any(|check| check.status == CliCheckStatus::Warn)
        {
            CliOverallStatus::NeedsProductionConfig
        } else {
            CliOverallStatus::Ready
        };

        Self {
            product: "opensnow".to_string(),
            lane: "opensnow-cli".to_string(),
            target: "enterprise-self-service-account-infra".to_string(),
            status,
            commands: command_specs(),
            config: CliConfigSpec {
                default_file: "opensnow.toml".to_string(),
                global_flags: vec!["--config <PATH>".to_string()],
                environment: vec![
                    "OPENSNOW_STORAGE_S3_BUCKET".to_string(),
                    "OPENSNOW_STORAGE_GCS_BUCKET".to_string(),
                    "OPENSNOW_STORAGE_AZURE_CONTAINER".to_string(),
                    "OPENSNOW_STORAGE_ACCESS_KEY".to_string(),
                    "OPENSNOW_STORAGE_SECRET_KEY".to_string(),
                    "OPENSNOW_OTEL_DISABLED".to_string(),
                ],
                secret_policy: "The CLI never prints secret values; production account/infra deployments must use workload identity or secret handles instead of inline credentials.".to_string(),
            },
            schemas: vec![
                "OpenSnowCliReport".to_string(),
                "CliCommandSpec".to_string(),
                "CliConfigSpec".to_string(),
                "CliAgentContract".to_string(),
                "CliReadinessCheck".to_string(),
            ],
            agent_contract: CliAgentContract {
                version: "opensnow-cli.v1".to_string(),
                required_scopes: vec!["opensnow:cli:read".to_string()],
                stable_commands: vec![
                    "opensnow cli contract --format json".to_string(),
                    "opensnow cli doctor --format json".to_string(),
                    "opensnow init --with-sample-data --industry=telecom|banking|both".to_string(),
                    "opensnow start --enable-pgwire".to_string(),
                    "opensnow local <SQL>".to_string(),
                    "opensnow shell -c <SQL>".to_string(),
                    "opensnow account-register --account-name <NAME> --owner-email <EMAIL>".to_string(),
                    "opensnow account-workspace-create --account-id <ID> --name <NAME>".to_string(),
                ],
                api_endpoints: vec![
                    "GET /health".to_string(),
                    "GET /status".to_string(),
                    "POST /api/v1/query".to_string(),
                    "POST /api/v1/ingest".to_string(),
                    "POST /api/v1/accounts".to_string(),
                    "POST /api/v1/accounts/{account_id}/workspaces".to_string(),
                ],
                output_formats: vec!["text".to_string(), "json".to_string()],
            },
            checks,
        }
    }

    pub fn render_text(&self) -> String {
        let mut out = format!(
            "OpenSnow CLI ({})\nTarget: {}\nStatus: {:?}\n\nCommands:\n",
            self.lane, self.target, self.status
        );
        for command in &self.commands {
            out.push_str(&format!(
                "- {} — {} (output: {})\n",
                command.name, command.purpose, command.output
            ));
        }
        out.push_str("\nReadiness:\n");
        for check in &self.checks {
            out.push_str(&format!(
                "- {}: {:?} — {}\n",
                check.id, check.status, check.evidence
            ));
            if let Some(remediation) = &check.remediation {
                out.push_str(&format!("  remediation: {remediation}\n"));
            }
        }
        out
    }
}

fn command_specs() -> Vec<CliCommandSpec> {
    vec![
        CliCommandSpec {
            name: "opensnow cli contract".to_string(),
            purpose: "Print the stable OpenSnow command-line and agent-facing contract.".to_string(),
            required_inputs: vec![],
            optional_inputs: vec!["--format text|json".to_string(), "--config <PATH>".to_string()],
            output: "OpenSnowCliReport".to_string(),
        },
        CliCommandSpec {
            name: "opensnow cli doctor".to_string(),
            purpose: "Validate local CLI configuration against enterprise self-service deployment expectations.".to_string(),
            required_inputs: vec![],
            optional_inputs: vec!["--format text|json".to_string(), "--config <PATH>".to_string()],
            output: "OpenSnowCliReport with readiness checks".to_string(),
        },
        CliCommandSpec {
            name: "opensnow init".to_string(),
            purpose: "Create local OpenSnow config and optional sample warehouse data.".to_string(),
            required_inputs: vec![],
            optional_inputs: vec!["--with-sample-data".to_string(), "--industry telecom|banking|both".to_string()],
            output: "Local files under opensnow.toml and the configured warehouse path".to_string(),
        },
        CliCommandSpec {
            name: "opensnow local".to_string(),
            purpose: "Run a SQL statement without starting the HTTP/pgwire server.".to_string(),
            required_inputs: vec!["<SQL>".to_string()],
            optional_inputs: vec!["--config <PATH>".to_string()],
            output: "Pretty-printed Arrow record batches".to_string(),
        },
        CliCommandSpec {
            name: "opensnow start".to_string(),
            purpose: "Start the OpenSnow server in standalone, coordinator, or worker mode.".to_string(),
            required_inputs: vec![],
            optional_inputs: vec!["--http-port <PORT>".to_string(), "--enable-pgwire".to_string(), "--role standalone|coordinator|worker".to_string()],
            output: "Long-running server process".to_string(),
        },
        CliCommandSpec {
            name: "opensnow account-register".to_string(),
            purpose: "Bootstrap an enterprise account, organization, workspace, owner membership, and service identity.".to_string(),
            required_inputs: vec!["--account-name <NAME>".to_string(), "--owner-email <EMAIL>".to_string()],
            optional_inputs: vec!["--config <PATH>".to_string()],
            output: "Enterprise account bootstrap identifiers".to_string(),
        },
    ]
}

fn pass(id: &str, title: &str, evidence: impl Into<String>) -> CliReadinessCheck {
    CliReadinessCheck {
        id: id.to_string(),
        title: title.to_string(),
        status: CliCheckStatus::Pass,
        evidence: evidence.into(),
        remediation: None,
    }
}

fn warn(
    id: &str,
    title: &str,
    evidence: impl Into<String>,
    remediation: impl Into<String>,
) -> CliReadinessCheck {
    CliReadinessCheck {
        id: id.to_string(),
        title: title.to_string(),
        status: CliCheckStatus::Warn,
        evidence: evidence.into(),
        remediation: Some(remediation.into()),
    }
}

fn fail(
    id: &str,
    title: &str,
    evidence: impl Into<String>,
    remediation: impl Into<String>,
) -> CliReadinessCheck {
    CliReadinessCheck {
        id: id.to_string(),
        title: title.to_string(),
        status: CliCheckStatus::Fail,
        evidence: evidence.into(),
        remediation: Some(remediation.into()),
    }
}

fn config_file_check(_config: &OpenSnowConfig) -> CliReadinessCheck {
    pass(
        "cli.contract",
        "OpenSnow CLI contract is available",
        "opensnow cli contract and opensnow cli doctor are implemented as OpenSnow-specific commands",
    )
}

fn storage_check(config: &OpenSnowConfig) -> CliReadinessCheck {
    let storage = &config.storage;
    if storage.s3_bucket.is_some()
        || storage.gcs_bucket.is_some()
        || storage.azure_container.is_some()
    {
        pass(
            "storage.object_store",
            "Object-store warehouse configured",
            "cloud object-store bucket/container is configured",
        )
    } else {
        warn(
            "storage.object_store",
            "Object-store warehouse configured",
            format!("local warehouse path is {}", storage.warehouse_path),
            "configure S3, GCS, or Azure Blob storage for enterprise account deployments",
        )
    }
}

fn catalog_check(config: &OpenSnowConfig) -> CliReadinessCheck {
    let path = config.catalog.path.trim();
    if path.is_empty() {
        fail(
            "catalog.durable",
            "Durable catalog path configured",
            "catalog.path is empty",
            "set [catalog].path to a durable SQLite path or managed catalog backend",
        )
    } else if path.contains("/tmp/") {
        warn(
            "catalog.durable",
            "Durable catalog path configured",
            format!("catalog.path uses temporary storage: {path}"),
            "move catalog.path to persistent storage before production launch",
        )
    } else {
        pass(
            "catalog.durable",
            "Durable catalog path configured",
            format!("catalog.path is {path}"),
        )
    }
}

fn enterprise_secret_provider_check(config: &OpenSnowConfig) -> CliReadinessCheck {
    let secret_provider = &config.enterprise.secret_provider;
    let provider = secret_provider.provider.trim();
    if secret_provider.enabled
        && matches!(
            provider,
            "aws-secrets-manager" | "gcp-secret-manager" | "vault"
        )
        && (!secret_provider.kms_key_arn.trim().is_empty() || provider == "vault")
    {
        pass(
            "enterprise.secret_provider",
            "External secret provider enforced",
            format!("provider={provider}"),
        )
    } else if matches!(config.enterprise.mode.as_str(), "test-instance" | "local") {
        warn(
            "enterprise.secret_provider",
            "External secret provider enforced",
            "local/test mode does not require external secret provider",
            "enable aws-secrets-manager, gcp-secret-manager, or vault before enterprise deployment",
        )
    } else {
        fail(
            "enterprise.secret_provider",
            "External secret provider enforced",
            format!("enabled={}, provider={provider}", secret_provider.enabled),
            "set [enterprise.secret_provider].enabled=true and use aws-secrets-manager, gcp-secret-manager, or vault",
        )
    }
}

fn entitlement_check(config: &OpenSnowConfig) -> CliReadinessCheck {
    if config.enterprise.entitlement_required
        && !config.enterprise.marketplace_provider.trim().is_empty()
    {
        pass(
            "enterprise.entitlements",
            "Marketplace entitlement source configured",
            format!("provider={}", config.enterprise.marketplace_provider),
        )
    } else if config.enterprise.entitlement_required {
        fail(
            "enterprise.entitlements",
            "Marketplace entitlement source configured",
            "entitlement_required=true but marketplace_provider is empty",
            "set [enterprise].marketplace_provider for AWS/GCP marketplace deployments",
        )
    } else {
        warn(
            "enterprise.entitlements",
            "Marketplace entitlement source configured",
            "entitlements are not required in this config",
            "enable entitlement_required for marketplace self-service deployments",
        )
    }
}

fn pgwire_policy_check(config: &OpenSnowConfig) -> CliReadinessCheck {
    if config.server.pg_enabled {
        pass(
            "server.pgwire",
            "pgwire exposure is explicit",
            format!("pgwire enabled on port {}", config.server.pg_port),
        )
    } else {
        pass(
            "server.pgwire",
            "pgwire exposure is explicit",
            "pgwire disabled by default; --enable-pgwire is required for trusted local access",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_contract_is_opensnow_cli_specific() {
        let report = OpenSnowCliReport::from_config(&OpenSnowConfig::default());

        assert_eq!(report.product, "opensnow");
        assert_eq!(report.lane, "opensnow-cli");
        assert_eq!(report.agent_contract.version, "opensnow-cli.v1");
        assert!(
            report
                .agent_contract
                .stable_commands
                .contains(&"opensnow cli contract --format json".to_string())
        );
        let json = serde_json::to_string(&report).expect("report serializes");
        assert!(!json.to_lowercase().contains("mcp finder"));
        assert!(!json.to_lowercase().contains("aether"));
    }
}
