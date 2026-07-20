from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_public_quickstart_exists_and_covers_required_paths():
    doc = ROOT / "docs" / "PUBLIC_TEST_PATH.md"
    text = doc.read_text()

    required_sections = [
        "# Public test path",
        "## 1. One-command public demo",
        "## 2. Local quickstart",
        "## 3. Docker Compose demo",
        "## 4. k3d Kubernetes demo",
        "## 5. First SQL query",
        "## 6. Pipeline and dashboard tab expectations",
        "## 7. Smoke checks",
        "## 8. SSO-ready org auth surface",
        "## 9. Cloud demo path",
        "## Remaining launch blockers",
    ]
    previous_index = -1
    for section in required_sections:
        assert section in text
        section_index = text.index(section)
        assert section_index > previous_index
        previous_index = section_index

    required_commands = [
        "cargo run -p opensnow-cli -- init --with-sample-data --industry both",
        "cargo run -p opensnow-cli -- start",
        "docker compose up --build opensnow",
        "k3d cluster create --config deploy/k3d-config.yaml",
        "helm upgrade --install opensnow deploy/helm/opensnow",
        "scripts/public-smoke.sh",
        "curl -fsS http://localhost:8080/health",
        "psql -h localhost -p 5433",
        "--enable-pgwire",
        "OPENSNOW_ENABLE_PGWIRE=1",
        "pg_enabled = true",
    ]
    for command in required_commands:
        assert command in text


def test_readme_points_external_users_to_public_test_path():
    readme = ROOT / "README.md"
    text = readme.read_text()
    assert "Try OpenSnow in 10 minutes" in text
    assert "docs/PUBLIC_TEST_PATH.md" in text
    assert "scripts/public-smoke.sh" in text


def test_architecture_pgwire_docs_match_external_demo_policy():
    text = (ROOT / "ARCHITECTURE.md").read_text()

    assert "PostgreSQL Wire (disabled by default; trusted-local 5433 only)" in text
    assert "pgwire is disabled by default for public/external demos" in text
    assert "trusted local" in text
    assert "--enable-pgwire" in text
    assert "localhost:5433" in text
    assert "PostgreSQL Wire (5432)" not in text
    assert "SQL at localhost:5432" not in text


def test_sql_compatibility_documents_external_demo_guardrails():
    text = (ROOT / "docs" / "SQL_COMPATIBILITY.md").read_text()

    assert "One SQL statement per request" in text
    assert "SQL text is limited to 64 KiB" in text
    assert "Requests time out after 30 seconds by default" in text
    assert "COPY INTO` is blocked on `/api/v1/query`" in text
    assert "Destructive DDL (`DROP TABLE`, `DROP MATERIALIZED VIEW`) is blocked" in text
    assert "Schema-qualified, catalog-qualified, path-like, or quoted targets are rejected before planning" in text

    supported_block = text.split("## REST sample workflow", 1)[0]
    assert "COPY INTO public_smoke" not in supported_block
    assert "DROP TABLE smoke_rollup" not in supported_block


def test_public_smoke_script_is_safe_and_checks_http_and_pgwire():
    script = ROOT / "scripts" / "public-smoke.sh"
    text = script.read_text()
    assert text.startswith("#!/usr/bin/env bash")
    assert "set -euo pipefail" in text
    assert "curl -fsS \"$BASE_URL/health\"" in text
    assert '"sql": f"SELECT COUNT(*) AS rows FROM {sys.argv[1]}"' in text
    assert "public_smoke_$$" in text
    assert "psql -h \"$PGHOST\" -p \"$PGPORT\"" in text
    assert "OPENSNOW_ENABLE_PGWIRE" in text
    assert "psycopg" in text
    assert "rm -rf /" not in text


def test_quickstart_smoke_builds_once_and_reuses_binary_for_local_mode():
    script = ROOT / "scripts" / "quickstart-smoke.sh"
    text = script.read_text()

    assert "cargo build --quiet --bin opensnow" in text
    assert 'opensnow_bin="${OPENSNOW_BIN:-target/debug/opensnow}"' in text
    assert "cargo run" not in text


def test_kubernetes_storage_credentials_never_render_into_configmap():
    configmap = ROOT / "deploy" / "helm" / "opensnow" / "templates" / "configmap.yaml"
    text = configmap.read_text()

    assert "s3_access_key =" not in text
    assert "s3_secret_key =" not in text
    assert "azure_account_key =" not in text
    assert "azure_client_secret =" not in text
    assert "gcs_service_account" not in text
    assert "OPENSNOW_STORAGE_ACCESS_KEY" not in text
    assert "OPENSNOW_STORAGE_SECRET_KEY" not in text


def test_demo_values_use_synthetic_non_secret_storage_defaults():
    values_dev = (ROOT / "deploy" / "helm" / "opensnow" / "values-dev.yaml").read_text()
    compose = (ROOT / "docker-compose.yml").read_text()
    sample = (ROOT / "opensnow.toml").read_text()

    forbidden_literals = [
        "minio" + "admin",
        "dev" + "password",
        "admin_password   = \"" + "admin" + "\"",
        "GF_SECURITY_ADMIN_PASSWORD: " + "admin",
    ]
    for literal in forbidden_literals:
        assert literal not in values_dev
        assert literal not in compose
        assert literal not in sample

    assert "existingSecret:" in values_dev
    # Dev values must use an obviously-synthetic placeholder credential, never a
    # real or real-looking secret.
    assert "CHANGE-ME-DEV-ONLY" in values_dev
    assert "OPENSNOW_DEMO_PASSWORD" in compose
    assert "OPENSNOW_DEMO_MINIO_ROOT_USER" in compose


def test_deployment_docs_cover_cloud_runtime_safety_and_recovery():
    doc = (ROOT / "docs" / "DEPLOYMENT.md").read_text()
    required_terms = [
        "OPENSNOW_STORAGE_ACCESS_KEY",
        "OPENSNOW_STORAGE_SECRET_KEY",
        "OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP",
        "IRSA",
        "Workload Identity",
        "existingSecret",
        "loadBalancerSourceRanges",
        "helm rollback opensnow",
        "kubectl rollout status deploy/opensnow-coordinator",
        "scripts/public-smoke.sh",
        "secret scan",
        "existing-secret",
        "generated secret",
        "local demo only",
        "hosted/cloud deployments",
    ]
    for term in required_terms:
        assert term in doc

    forbidden_copy_paste_defaults = [
        'admin_password = "admin"',
        'admin_password    = "admin"',
        'password=admin',
        'password="admin"',
        'password: admin',
    ]
    for literal in forbidden_copy_paste_defaults:
        assert literal not in doc

    assert 'admin_password = "<generated-secret>"' in doc
    assert "OPENSNOW_AUTH_ADMIN_PASSWORD=$(openssl rand -base64 32)" in doc
    assert 'Authorization: Bearer <admin-access-token>' in doc
    assert 'Authorization: Bearer <admin...en>' not in doc
    assert 'Authorization: Bearer ***' not in doc


def test_public_demo_has_one_command_entrypoint_manifest_seed_and_reset():
    demo = ROOT / "scripts" / "demo.sh"
    seed = ROOT / "scripts" / "demo-seed.py"
    manifest = ROOT / "demo" / "public-demo-manifest.json"
    doc = ROOT / "docs" / "PUBLIC_DEMO.md"

    assert demo.exists()
    assert seed.exists()
    assert manifest.exists()
    assert doc.exists()

    demo_text = demo.read_text()
    assert demo_text.startswith("#!/usr/bin/env bash")
    assert "set -euo pipefail" in demo_text
    assert "demo/public-demo-manifest.json" in demo_text
    assert "demo-seed.py" in demo_text
    assert "reset)" in demo_text
    assert "OPENSNOW_JWT_SECRET" not in demo_text
    assert "rm -rf /" not in demo_text
    assert "scripts/demo.sh" in doc.read_text()
    assert "scripts/demo.sh reset" in doc.read_text()


def test_demo_pgwire_requires_explicit_server_enable_separate_from_smoke_skip():
    demo_text = (ROOT / "scripts" / "demo.sh").read_text()
    public_demo = (ROOT / "docs" / "PUBLIC_DEMO.md").read_text()
    readme = (ROOT / "README.md").read_text()

    assert "OPENSNOW_ENABLE_PGWIRE=1" in demo_text
    assert "--enable-pgwire" in demo_text
    assert 'OPENSNOW_ENABLE_PGWIRE="${OPENSNOW_ENABLE_PGWIRE:-0}"' in demo_text
    assert '"$OPENSNOW_ENABLE_PGWIRE" = "1"' in demo_text
    assert "ENABLE_PGWIRE" not in demo_text.replace("OPENSNOW_ENABLE_PGWIRE", "")
    assert 'OPENSNOW_SKIP_PG="${OPENSNOW_SKIP_PG:-1}"' in demo_text
    assert "OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/demo.sh" in public_demo
    assert "OPENSNOW_SKIP_PG=0 scripts/demo.sh" not in public_demo.replace(
        "OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/demo.sh", ""
    )
    assert "OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/demo.sh" in readme


def test_public_demo_manifest_is_deterministic_synthetic_and_secret_free():
    import json

    manifest_path = ROOT / "demo" / "public-demo-manifest.json"
    manifest = json.loads(manifest_path.read_text())

    assert manifest["name"] == "opensnow-public-demo"
    assert manifest["synthetic"] is True
    assert manifest["deterministic"] is True
    assert manifest["source"] == "generated"
    assert manifest["contains_real_customer_data"] is False
    assert manifest["seed"] == 424242
    assert manifest["reset"]["command"] == "scripts/demo.sh reset"

    table_names = {table["name"] for table in manifest["tables"]}
    assert table_names == {"demo_customers", "demo_orders", "demo_regions"}

    for table in manifest["tables"]:
        assert table["row_count"] == len(table["rows"])
        assert table["row_count"] > 0
        assert table["columns"]
        for row in table["rows"]:
            assert len(row) == len(table["columns"])

    serialized = manifest_path.read_text().lower()
    banned = ["password", "passwd", "api_key", "apikey", "secret", "token", "private_key"]
    assert not any(word in serialized for word in banned)


def test_public_demo_docs_and_readme_document_stable_paths_and_cleanup():
    public_demo = (ROOT / "docs" / "PUBLIC_DEMO.md").read_text()
    public_path = (ROOT / "docs" / "PUBLIC_TEST_PATH.md").read_text()
    readme = (ROOT / "README.md").read_text()

    for text in (public_demo, public_path, readme):
        assert "scripts/demo.sh" in text
        assert "demo/public-demo-manifest.json" in text
        assert "scripts/demo.sh reset" in text

    assert ".opensnow-demo/" in public_demo
    assert "demo_customers" in public_demo
    assert "demo_orders" in public_demo
    assert "demo_regions" in public_demo


def test_public_oss_demo_assets_do_not_ship_operator_hosting_identifiers():
    scanned_paths = [
        ROOT / "deploy" / "cloudrun" / "release.sh",
        ROOT / "deploy" / "demo" / "README.md",
        ROOT / "deploy" / "demo" / "docker-compose.yml",
        ROOT / "deploy" / "demo" / "opensnow.demo.toml",
        ROOT / "deploy" / "demo" / "metabase-build-dashboards.py",
        ROOT / "deploy" / "demo" / "metabase-build-krona.py",
        ROOT / "deploy" / "demo" / "metabase-krona-narrate.py",
        ROOT / "deploy" / "demo" / "metabase-krona-v2.py",
        ROOT / "deploy" / "demo" / "metabase-krona-v3.py",
        ROOT / "deploy" / "demo" / "metabase-setup.py",
        ROOT / "deploy" / "terraform" / "hetzner" / "outputs.tf",
        ROOT / "deploy" / "terraform" / "hetzner" / "README.md",
        ROOT / "deploy" / "terraform" / "hetzner" / "terraform.tfvars.example",
        ROOT / "deploy" / "terraform" / "hetzner" / "variables.tf",
        ROOT / "deploy" / "terraform" / "oci" / "main.tf",
        ROOT / "deploy" / "terraform" / "oci" / "outputs.tf",
        ROOT / "deploy" / "terraform" / "oci" / "README.md",
        ROOT / "deploy" / "terraform" / "oci" / "terraform.tfvars.example",
        ROOT / "deploy" / "terraform" / "oci" / "variables.tf",
        ROOT / "docs" / "CHATGPT_APP_ALIGNMENT.md",
        ROOT / "docs" / "MCP_CONTROL_PLANE.md",
        ROOT / "docs" / "PRIVACY_POLICY.md",
        ROOT / "docs" / "SECURITY_TEST_REPORT.md",
        ROOT / "crates" / "opensnow-agent" / "src" / "platform_tools.rs",
        ROOT / "crates" / "opensnow-server" / "src" / "rest.rs",
        ROOT / "crates" / "opensnow-server" / "static" / "app.html",
    ]
    forbidden_literals = [
        "opensnow.ebdsweden.com",
        "metabase.ebdsweden.com",
        "EBD-Sweden/docs",
        "opensnow-prod",
        "OPENSNOW_VM",
        "00769301-ca5e-49b9-8626-8ce33dd01ea9",
    ]

    for path in scanned_paths:
        text = path.read_text()
        for literal in forbidden_literals:
            assert literal not in text, f"{path.relative_to(ROOT)} contains operator-only {literal!r}"
