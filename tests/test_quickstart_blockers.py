from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def test_docker_quickstart_is_http_only_by_default_and_uses_non_root_home():
    compose = (ROOT / "docker-compose.yml").read_text()
    pgwire_override = (ROOT / "docker-compose.pgwire.yml").read_text()
    dockerfile = (ROOT / "Dockerfile").read_text()
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()

    assert '"127.0.0.1:8080:8080"' in compose
    assert "5433" not in compose
    assert '"8080:8080"' not in compose
    assert '"127.0.0.1:5433:5433"' in pgwire_override
    assert "OPENSNOW_ENABLE_PGWIRE: \"1\"" in pgwire_override
    assert "ENV HOME=/home/opensnow" in dockerfile
    assert "WORKDIR /home/opensnow" in dockerfile
    assert "/home/opensnow/.opensnow" in deployment
    install_block = deployment.split("### Public demo path", 1)[0]
    # The simple `docker run` quickstart (Option C) must bind loopback HTTP only
    # and must not expose the pgwire port by default.
    default_docker_block = install_block.split("# Option C: Docker", 1)[1].split("### Start", 1)[0]
    assert "-p 127.0.0.1:8080:8080" in default_docker_block
    assert "-p 127.0.0.1:5433:5433" not in default_docker_block
    assert "/root/.opensnow" not in deployment


def test_deployment_docs_scope_pgwire_to_trusted_explicit_opt_in():
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()

    assert "# SQL:     localhost:5433 (PostgreSQL wire protocol)" not in deployment
    assert "OPENSNOW_SERVER_PG_PORT=5432" not in deployment
    assert "OpenSnow speaks PostgreSQL wire protocol — use any PG client:" not in deployment
    assert "Same binary, same SQL, same wire protocol everywhere." not in deployment
    assert "pgwire is disabled by default" in deployment
    assert "trusted local" in deployment
    assert "--enable-pgwire" in deployment
    assert "OPENSNOW_ENABLE_PGWIRE=1" in deployment
    assert "OPENSNOW_SERVER_PG_PORT=5433" in deployment


def dockerignore_patterns() -> set[str]:
    dockerignore = (ROOT / ".dockerignore").read_text()
    return {
        line.strip().removesuffix("/")
        for line in dockerignore.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    }


def test_docker_build_context_keeps_workspace_member_manifests_and_test_assets():
    cargo_toml = (ROOT / "Cargo.toml").read_text()
    ignored_paths = dockerignore_patterns()

    workspace_members = re.findall(r'"([^"]+)"', cargo_toml.split("resolver =", 1)[0])

    for member in workspace_members:
        assert member not in ignored_paths, f"workspace member {member} is excluded from Docker build context"
        assert f"{member}/Cargo.toml" not in ignored_paths

    for required_path in ["docs", "scripts", ".github", "*.md"]:
        assert required_path not in ignored_paths, f"{required_path} is needed for Docker-context validation"


def test_docker_build_context_excludes_local_agent_demo_state_and_secrets():
    ignored_paths = dockerignore_patterns()

    for forbidden_path in [
        ".hermes",
        ".opensnow-demo",
        ".opensnow",
        ".env",
        ".env.*",
        "*.db",
        "*.sqlite",
        "*.sqlite3",
        "*.log",
        "*.pid",
    ]:
        assert forbidden_path in ignored_paths


def test_helm_k3d_docs_use_consumed_chart_values_and_config_cluster_name():
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()
    k3d = (ROOT / "deploy" / "k3d-config.yaml").read_text()

    assert "k3d cluster create --config deploy/k3d-config.yaml" in deployment
    assert "k3d cluster create opensnow --config" not in deployment
    assert "metadata:\n  name: opensnow-dev" in k3d
    assert "-f deploy/helm/opensnow/values-dev.yaml" in deployment
    assert "--set config.storage.type=s3" in deployment
    assert "--set config.storage.endpoint=http://opensnow-minio:9000" in deployment
    assert "--set worker.replicas=3" in deployment
    assert "--set workers.replicas" not in deployment
    assert "--set storage.type" not in deployment


def test_sso_admin_curl_examples_are_well_formed_and_match_routes():
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()
    admin = (ROOT / "crates" / "opensnow-server" / "src" / "admin.rs").read_text()

    for route in [
        "/api/v1/admin/tenants",
        "/api/v1/admin/tenants/{tenant_id}/sso-mappings",
        "/api/v1/auth/sso/login",
    ]:
        assert route in deployment
        assert route in admin

    assert "Authorization: Bearer" in deployment
    assert deployment.count('-H "Authorization: Bearer <admin-access-token>" \\') >= 2
    assert '-H "content-type: application/json"' in deployment
    assert deployment.count('-H "content-type: application/json"') >= 2
    assert 'Authorization: Bearer <admin-access-token>"\n' not in deployment

    curl_blocks = re.findall(r"```bash\n(curl .*?)\n```", deployment, flags=re.DOTALL)
    assert curl_blocks, "expected runnable curl examples in deployment docs"
    for block in curl_blocks:
        if "/api/v1/admin/" in block:
            assert block.count('"') % 2 == 0, block


def test_enterprise_auth_qa_docs_match_public_sso_domain_contract():
    qa_doc = (ROOT / "docs" / "ENTERPRISE_AUTH_QA_VALIDATION.md").read_text()

    assert "sso_not_configured_for_domain" in qa_doc
    assert "jq -e '.error == \"sso_not_configured_for_domain\"'" in qa_doc
    assert "jq -e '.error == \"sso_backend_not_configured\"'" not in qa_doc
    assert "(.error != \"sso_backend_not_configured\")" in qa_doc
    assert "grep -E '401|403|422' /tmp/opensnow-sso-negative.txt" not in qa_doc
    assert "`Authorization: Bearer *** Supported operations" not in qa_doc
    assert "`Authorization: Bearer ***`. Supported operations" in qa_doc


def test_quickstart_smoke_script_exists_and_is_ci_backed():
    script = ROOT / "scripts" / "quickstart-smoke.sh"
    ci = ROOT / ".github" / "workflows" / "ci.yml"
    deployment = ROOT / "docs" / "DEPLOYMENT.md"

    text = script.read_text()
    assert text.startswith("#!/usr/bin/env bash")
    assert "set -euo pipefail" in text
    assert "--mode local|docker|k3d" in text
    assert "init --with-sample-data --industry both" in text
    assert "shell -c" in text
    assert "/api/v1/status" in text
    assert "/api/v1/query" in text
    assert "psql -h" in text
    assert "docker compose" in text
    assert "docker compose run --rm opensnow" in text
    assert "k3d cluster create --config deploy/k3d-config.yaml" in text
    assert "helm upgrade --install opensnow deploy/helm/opensnow" in text

    assert "scripts/quickstart-smoke.sh --mode local" in ci.read_text()
    assert "scripts/quickstart-smoke.sh --mode local" in deployment.read_text()
    assert "scripts/quickstart-smoke.sh --mode docker" in deployment.read_text()
    assert "scripts/quickstart-smoke.sh --mode k3d" in deployment.read_text()


def test_public_test_path_uses_safe_k3d_command_and_chart_values():
    public_path = (ROOT / "docs" / "PUBLIC_TEST_PATH.md").read_text()

    assert "k3d cluster create --config deploy/k3d-config.yaml" in public_path
    assert "k3d cluster create opensnow --config" not in public_path
    assert "-f deploy/helm/opensnow/values-dev.yaml" in public_path
    assert "--set config.storage.type=s3" in public_path
    assert "--set config.storage.endpoint=http://opensnow-minio:9000" in public_path
    assert "--set worker.replicas=3" in public_path


def test_public_test_path_evaluation_curl_examples_are_copy_paste_safe():
    public_path = (ROOT / "docs" / "PUBLIC_TEST_PATH.md").read_text()
    evaluation_section = public_path.split("## 1.1 Hosted evaluation sandbox account mode", 1)[1].split("## 2. Local quickstart", 1)[0]

    bearer_expr = "Bearer ${OPENSNOW_ADMIN_TOKEN:?set OPENSNOW_ADMIN_TOKEN}"
    bearer_header = f'-H "authorization: {bearer_expr}"'
    broken_bearer_header = "authorization: Bearer " + "*" * 3

    assert broken_bearer_header not in evaluation_section
    assert bearer_expr in evaluation_section
    assert bearer_header in evaluation_section
    assert evaluation_section.count('"') % 2 == 0
    assert "generated `eval-*` tenant" in evaluation_section

    curl_blocks = re.findall(r"```bash\n(curl .*?)\n```", evaluation_section, flags=re.DOTALL)
    assert curl_blocks, "expected evaluation curl examples"
    for block in curl_blocks:
        assert block.count('"') % 2 == 0, block
        for line in block.splitlines():
            if "authorization: Bearer" in line:
                assert bearer_header in line

def test_client_compatibility_pack_documents_and_smokes_supported_lanes():
    public_path = (ROOT / "docs" / "PUBLIC_TEST_PATH.md").read_text()
    sql_doc = (ROOT / "docs" / "SQL_COMPATIBILITY.md").read_text()
    smoke = (ROOT / "scripts" / "public-smoke.sh").read_text()
    ui = (ROOT / "crates" / "opensnow-server" / "static" / "index.html").read_text()

    for lane in ["psql", "psycopg", "psycopg2", "dbt", "information_schema", "pg_catalog", "BI introspection"]:
        assert lane in sql_doc
    assert "Client compatibility support matrix" in sql_doc
    assert "extended query protocol" in sql_doc
    assert "COPY protocol" in sql_doc
    assert "public hosted pgwire remains disabled" in sql_doc

    for smoke_probe in [
        "SELECT COUNT(*) AS rows",
        "information_schema.tables",
        "information_schema.columns",
        "/api/v1/dbt/catalog",
        "COPY $TABLE TO STDOUT",
    ]:
        assert smoke_probe in smoke
    assert "psycopg" in smoke and "psycopg2" in smoke
    assert "client_name, module_name" in smoke
    assert "for client_name, module_name in" in smoke
    assert '("psycopg", "psycopg")' in smoke
    assert '("psycopg2", "psycopg2")' in smoke
    assert "python pg client psycopg skipped" in smoke
    assert "python pg client psycopg2 skipped" in smoke
    assert "extended query protocol" in smoke

    assert "simple-query only" in public_path
    assert "Client compatibility matrix" in public_path
    assert "simple-query only" in ui
    assert "BI/dbt introspection" in ui


def test_public_smoke_copy_assertion_rejects_generic_copy_parser_errors():
    smoke = (ROOT / "scripts" / "public-smoke.sh").read_text()

    assert "copy_expected_error_pattern=" in smoke
    pattern_match = re.search(r'^\s*copy_expected_error_pattern="([^"]+)"$', smoke, flags=re.MULTILINE)
    assert pattern_match, "public smoke should centralize the COPY clear-error pattern"

    pattern = pattern_match.group(1)
    generic_parser_error = 'ERROR: syntax error at or near "COPY"'
    assert re.search(pattern, generic_parser_error, flags=re.IGNORECASE) is None

    for clear_error in [
        "SQL_COMPATIBILITY: COPY is not implemented; use REST /api/v1/ingest",
        "COPY protocol is unsupported in public smoke; use /api/v1/ingest",
        "COPY is not-supported by OpenSnow pgwire",
        "COPY is not supported by OpenSnow pgwire",
    ]:
        assert re.search(pattern, clear_error, flags=re.IGNORECASE), clear_error

    copy_assertion_block = smoke.split("PostgreSQL wire unsupported COPY", 1)[1].split(
        "PostgreSQL wire Python client lane", 1
    )[0]
    assert 'grep -q "COPY\\|' not in copy_assertion_block


def test_helm_default_metadata_password_is_generated_not_deterministic():
    values = (ROOT / "deploy" / "helm" / "opensnow" / "values.yaml").read_text()
    values_dev = (ROOT / "deploy" / "helm" / "opensnow" / "values-dev.yaml").read_text()
    configmap = (
        ROOT / "deploy" / "helm" / "opensnow" / "templates" / "configmap.yaml"
    ).read_text()
    regression = ROOT / "scripts" / "helm-default-secret-check.sh"

    forbidden = "OPEN/SNOW/DEMO/ONLY/METADATA"
    assert forbidden not in values
    assert forbidden not in values_dev
    assert "T1BFTi9TTk9XL0RFTU8vT05MWS9NRVRBREFUQQ==" not in configmap
    assert ".Values.metadata.builtin.password | b64enc" not in configmap
    assert "lookup \"v1\" \"Secret\"" in configmap
    assert "randAlphaNum" in configmap
    assert regression.exists()
    regression_text = regression.read_text()
    assert forbidden in regression_text
    assert "alpine/helm:3.14.0" in regression_text
    assert "base64.b64decode" in regression_text


def test_helm_pgwire_service_and_notes_are_opt_in_only():
    gateway_service = (
        ROOT / "deploy" / "helm" / "opensnow" / "templates" / "gateway-service.yaml"
    ).read_text()
    notes = (ROOT / "deploy" / "helm" / "opensnow" / "templates" / "NOTES.txt").read_text()
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()

    postgres_port_block = gateway_service.split("- name: postgres", 1)[0]
    assert "{{- if .Values.coordinator.pgwireEnabled }}" in postgres_port_block
    assert gateway_service.count("{{- if .Values.coordinator.pgwireEnabled }}") == 1
    assert "nodePort: 30543" in gateway_service.split(
        "{{- if .Values.coordinator.pgwireEnabled }}", 1
    )[1].split("{{- end }}", 1)[0]

    assert "PostgreSQL wire protocol is disabled" in notes
    assert "--set coordinator.pgwireEnabled=true" in notes
    assert notes.count("{{- if .Values.coordinator.pgwireEnabled }}") >= 2
    for psql_line in [line for line in notes.splitlines() if "psql -h" in line]:
        prefix = notes.split(psql_line, 1)[0]
        assert prefix.rfind("{{- if .Values.coordinator.pgwireEnabled }}") > prefix.rfind("{{- end }}")

    assert "`coordinator.pgwireEnabled` | `false`" in deployment
    assert "--set coordinator.pgwireEnabled=true" in deployment
    assert "kubectl port-forward svc/opensnow-gateway 8080:8080 5433:5433" not in deployment


def test_enterprise_release_is_hard_gated_as_oidc_only_until_saml_ships():
    qa_doc = (ROOT / "docs" / "ENTERPRISE_AUTH_QA_VALIDATION.md").read_text()
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()
    architecture = (ROOT / "ARCHITECTURE.md").read_text()
    enterprise_values = (
        ROOT / "deploy" / "helm" / "opensnow" / "values-enterprise-aws.yaml"
    ).read_text()

    for doc in [qa_doc, deployment, architecture, enterprise_values]:
        assert "OIDC-only" in doc
        assert "saml_unsupported_fail_closed" in doc
        assert "native SAML" in doc

    forbidden_native_saml_claims = [
        "SAML SSO | Dex (translates SAML",
        "SAML SSO | Public-platform requirement",
        "native SAML support",
        "native SAML readiness",
    ]
    combined = "\n".join([qa_doc, deployment, architecture, enterprise_values])
    for forbidden in forbidden_native_saml_claims:
        assert forbidden not in combined

    enterprise_section = deployment.split("## Enterprise BYOC / marketplace deployment", 1)[1]
    assert not re.search(r"customer IdP\s+OIDC/SAML inputs", enterprise_section)
    assert re.search(r"customer IdP\s+OIDC inputs", enterprise_section)


def test_enterprise_helm_example_requires_safe_byoc_values():
    values = (ROOT / "deploy" / "helm" / "opensnow" / "values.yaml").read_text()
    enterprise = (
        ROOT / "deploy" / "helm" / "opensnow" / "values-enterprise-aws.yaml"
    ).read_text()
    configmap = (
        ROOT / "deploy" / "helm" / "opensnow" / "templates" / "configmap.yaml"
    ).read_text()
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()

    for required in [
        "enterprise:",
        'mode: "test-instance"',
        "requireExternalSecrets: true",
        "marketplace:",
        "entitlements:",
        "oidc:",
        "scim:",
        "auditExport:",
        "sealedSecrets:",
        "tls:",
    ]:
        assert required in values

    assert 'mode: "aws-marketplace"' in enterprise
    assert 'provider: "aws"' in enterprise
    assert "requireExternalSecrets: true" in enterprise
    assert "external:" in enterprise and "enabled: true" in enterprise
    assert "pgwireEnabled: false" in enterprise
    assert "existingSecret:" in enterprise
    assert "loadBalancerSourceRanges:" in enterprise
    assert "enterprise/BYOC renders require metadata.external.enabled=true" in configmap
    for guard in [
        "enterprise.marketplace.provider",
        "enterprise.marketplace.productCode",
        "enterprise.marketplace.customerIdentifier",
        "enterprise.marketplace.entitlementId",
        "enterprise.oidc.enabled=true",
        "enterprise.oidc.issuer",
        "enterprise.oidc.clientId",
        "enterprise.oidc.existingSecret",
        "enterprise.scim.enabled=true",
        "enterprise.scim.baseUrl",
        "enterprise.scim.tokenExistingSecret",
        "enterprise.auditExport.enabled=true",
        "enterprise.auditExport.bucket",
        "enterprise.sealedSecrets.enabled=true",
        "enterprise.sealedSecrets.provider",
        "enterprise.sealedSecrets.kmsKeyArn",
        "enterprise.tls.existingSecret",
        "gateway.loadBalancerSourceRanges",
    ]:
        assert guard in configmap
    assert "marketplace_product_code" in configmap
    assert "marketplace_customer_identifier" in configmap
    assert "customer-owned AWS account" in deployment
    assert "OpenSnow test-instance mode" in deployment


def test_enterprise_helm_marketplace_entitlement_id_fails_closed_even_when_entitlements_disabled():
    configmap = (
        ROOT / "deploy" / "helm" / "opensnow" / "templates" / "configmap.yaml"
    ).read_text()

    entitlement_guard = next(
        line
        for line in configmap.splitlines()
        if "enterprise.marketplace.entitlementId" in line and "fail" not in line
    )
    assert "enterprise.entitlements.required" not in entitlement_guard
    assert "enterprise.marketplace.enabled" in entitlement_guard
    assert 'eq $enterpriseMode "aws-marketplace"' in entitlement_guard


def test_enterprise_helm_metadata_external_secret_fails_closed_even_when_toggle_disabled():
    configmap = (
        ROOT / "deploy" / "helm" / "opensnow" / "templates" / "configmap.yaml"
    ).read_text()

    metadata_guard = next(
        line
        for line in configmap.splitlines()
        if "metadata.builtin.enabled" in line and "fail" not in line
    )
    assert "enterprise.requireExternalSecrets" not in metadata_guard
    assert "metadata.builtin.enabled" in metadata_guard
    assert "metadata.external.enabled" in metadata_guard
    assert "metadata.external.existingSecret" in metadata_guard


def test_terraform_aws_byoc_variables_cover_enterprise_infra_knobs():
    variables = (ROOT / "deploy" / "terraform" / "variables.tf").read_text()
    main_tf = (ROOT / "deploy" / "terraform" / "main.tf").read_text()
    readme = (ROOT / "deploy" / "terraform" / "README.md").read_text()

    for required in [
        'variable "deployment_mode"',
        'validation {',
        'condition     = contains(["test-instance", "enterprise", "aws-marketplace"], var.deployment_mode)',
        'variable "create_rds"',
        'variable "rds_secret_arn"',
        'variable "kms_key_arn"',
        'variable "oidc_issuer_url"',
        'variable "scim_enabled"',
        'variable "audit_export_bucket"',
        'variable "pgwire_exposure"',
    ]:
        assert required in variables

    assert "aws_kms_key" in main_tf
    assert "aws_db_instance" in main_tf
    assert "pgwire_exposure" in main_tf
    assert "marketplace_entitlement" in main_tf
    assert "local.rds_secret_arn" in main_tf
    assert 'var.rds_secret_arn != "" ? var.rds_secret_arn' in main_tf
    assert "terraform validate" in readme
    assert "customer-owned AWS account" in readme


def test_aws_byoc_terraform_has_private_rds_kms_audit_and_least_privilege_iam():
    main_tf = (ROOT / "deploy" / "terraform" / "main.tf").read_text()
    outputs = (ROOT / "deploy" / "terraform" / "outputs.tf").read_text()
    readme = (ROOT / "deploy" / "terraform" / "README.md").read_text()

    for required in [
        'resource "aws_security_group" "rds"',
        'referenced_security_group_id = module.eks.node_security_group_id',
        'resource "aws_s3_bucket_versioning" "audit_export"',
        'resource "aws_s3_bucket_object_lock_configuration" "audit_export"',
        'mode = "GOVERNANCE"',
        '"kms:Decrypt"',
        '"kms:GenerateDataKey"',
        '"kms:Encrypt"',
        'aws_s3_bucket.audit_export.arn',
        '"${aws_s3_bucket.audit_export.arn}/*"',
    ]:
        assert required in main_tf

    assert re.search(r"publicly_accessible\s*=\s*false", main_tf)
    assert re.search(r"backup_retention_period\s*=\s*7", main_tf)
    assert 'output "metadata_rds_master_secret_arn"' in outputs
    assert "sensitive   = true" in outputs
    assert "No static AWS access keys are rendered into Helm" in readme
    assert "backup" in readme.lower() and "restore" in readme.lower()
    assert "rollback" in readme.lower() and "uninstall" in readme.lower()


def test_aws_marketplace_helm_values_do_not_inline_credentials_or_public_pgwire():
    values = (ROOT / "deploy" / "helm" / "opensnow" / "values.yaml").read_text()
    enterprise = (
        ROOT / "deploy" / "helm" / "opensnow" / "values-enterprise-aws.yaml"
    ).read_text()
    deployment = (ROOT / "docs" / "DEPLOYMENT.md").read_text()
    chart = (ROOT / "deploy" / "helm" / "opensnow" / "Chart.yaml").read_text()

    assert "type: ClusterIP" in values
    assert "type: LoadBalancer" not in values.split("## Configuration", 1)[0]
    assert 'mode: "aws-marketplace"' in enterprise
    assert "eks.amazonaws.com/role-arn" in enterprise
    assert "existingSecret: \"opensnow-rds\"" in enterprise
    assert "access_key:" not in enterprise
    assert "secret_key:" not in enterprise
    assert "pgwireEnabled: false" in enterprise
    assert "loadBalancerSourceRanges:" in enterprise
    assert "service.beta.kubernetes.io/aws-load-balancer-ssl-cert" in enterprise
    assert "postgresql:\n  # Disabled by default" in values
    assert "condition: postgresql.enabled" in chart
    assert "cosign verify" in deployment
    assert "syft" in deployment
    assert "AWS Marketplace/BYOC remains gated" in deployment


def test_terraform_working_dir_ignores_generated_artifacts_and_tracks_lockfile_decision():
    gitignore = (ROOT / ".gitignore").read_text()
    readme = (ROOT / "deploy" / "terraform" / "README.md").read_text()

    assert "deploy/terraform/.terraform/" in gitignore
    assert "deploy/terraform/.terraform.lock.hcl" not in gitignore
    assert ".terraform.lock.hcl is intentionally tracked" in readme
    assert "Do not commit deploy/terraform/.terraform/" in readme
