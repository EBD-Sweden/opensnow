from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]
CHART = ROOT / "deploy" / "helm" / "opensnow"
SERVER_ADMIN = ROOT / "crates" / "opensnow-server" / "src" / "admin.rs"


def _load_values(name: str) -> dict:
    return yaml.safe_load((CHART / name).read_text())


def _deep_merge(base: dict, override: dict) -> dict:
    merged = dict(base)
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(merged.get(key), dict):
            merged[key] = _deep_merge(merged[key], value)
        else:
            merged[key] = value
    return merged


def _configmap_template() -> str:
    return (CHART / "templates" / "configmap.yaml").read_text()


def _assert_enterprise_values_are_safe(values: dict, provider: str) -> None:
    enterprise = values["enterprise"]
    assert enterprise["mode"] in {"aws-marketplace", "gcp-marketplace", "enterprise"}
    assert enterprise["sealedSecrets"]["enabled"] is True
    assert enterprise["sealedSecrets"]["provider"] == provider
    assert enterprise["sealedSecrets"]["kmsKeyArn"]
    assert enterprise["oidc"]["enabled"] is True
    assert enterprise["oidc"]["existingSecret"]
    assert enterprise["scim"]["enabled"] is True
    assert enterprise["scim"]["tokenExistingSecret"]
    assert enterprise["auditExport"]["enabled"] is True
    assert values["metadata"]["external"]["enabled"] is True
    assert values["metadata"]["external"]["existingSecret"]
    assert values["metadata"]["builtin"]["enabled"] is False
    assert not values["config"]["storage"].get("access_key")
    assert not values["config"]["storage"].get("secret_key")
    assert values["coordinator"]["pgwireEnabled"] is False


def _assert_enterprise_jwt_guard_inputs(values: dict) -> None:
    jwt = values["enterprise"]["jwt"]
    assert jwt["mode"] != "local_hs256"
    assert jwt["issuer"]
    assert jwt["audience"]
    assert jwt["kid"]
    assert jwt["existingSecret"]


def test_static_enterprise_secret_provider_template_has_runtime_config_and_fail_closed_guards():
    template = _configmap_template()
    for guard in [
        "enterprise.secret_provider.enabled=true",
        "enterprise.secret_provider.provider",
        "enterprise.secret_provider.kms_key_arn",
        "enterprise/BYOC renders must use workload identity or secret handles",
    ]:
        assert guard in template
    assert "[enterprise.secret_provider]" in template
    assert "provider = {{ .Values.enterprise.sealedSecrets.provider | quote }}" in template
    assert "kms_key_arn = {{ .Values.enterprise.sealedSecrets.kmsKeyArn | quote }}" in template


def test_static_oidc_callback_uses_shared_auth_state_jwt_manager_not_local_secret():
    admin = SERVER_ADMIN.read_text()
    callback = admin.split("async fn sso_callback", 1)[1].split("#[cfg(test)]", 1)[0]
    assert "JwtManager::new" not in callback
    assert "OPENSNOW_JWT_SECRET" not in callback
    assert "product_jwt_secret_required" not in callback
    compact = "".join(callback.split())
    assert "auth_state.jwt" in compact


def test_static_aws_and_gcp_enterprise_values_use_external_secret_handles_only():
    base = _load_values("values.yaml")
    aws = _deep_merge(base, _load_values("values-enterprise-aws.yaml"))
    gcp = _deep_merge(base, _load_values("values-enterprise-gcp.yaml"))

    _assert_enterprise_values_are_safe(aws, "aws-secrets-manager")
    _assert_enterprise_values_are_safe(gcp, "gcp-secret-manager")
    _assert_enterprise_jwt_guard_inputs(aws)
    _assert_enterprise_jwt_guard_inputs(gcp)
