# OpenSnow Deployment Guide

Complete deployment reference for all environments.
OpenSnow runs identically everywhere — the same binary and SQL surface, with HTTP enabled by default and pgwire disabled by default. Explicit pgwire enablement is trusted-local when auth is disabled, or bearer-JWT authenticated when enterprise/JWT auth is enabled.

---

## Table of Contents

0. [Production launch plan](#0-production-launch-plan)
1. [Local / On-Prem (Single Node)](#1-local--on-prem-single-node)
2. [On-Prem Kubernetes (Multi-Node)](#2-on-prem-kubernetes-multi-node)
3. [AWS (EKS + S3 + RDS)](#3-aws-eks--s3--rds)
4. [GCP (GKE + GCS + Cloud SQL)](#4-gcp-gke--gcs--cloud-sql)
5. [Azure (AKS + Blob + PostgreSQL)](#5-azure-aks--blob--postgresql)
6. [Configuration Reference](#6-configuration-reference)
7. [Database Setup](#7-database-setup)
8. [SSO / OIDC Setup](#8-sso--oidc-setup)
9. [Connecting Clients](#9-connecting-clients)
10. [Monitoring](#10-monitoring)

---

## 0. Before you deploy

For external users who only need to try OpenSnow quickly, start with `docs/PUBLIC_TEST_PATH.md` and run the CI-backed quickstart regression with `scripts/quickstart-smoke.sh --mode local`. Treat any internet-exposed deployment as production: enable authentication and TLS and supply secrets from a managed secret store (see below and `SECURITY.md`).

OpenSnow command-line automation uses the `opensnow-cli` contract:

```bash
opensnow cli contract --format json
opensnow cli doctor --format json
```

The stable schema is `OpenSnowCliReport`; see `docs/CLI.md`. The contract describes command inputs, config/env surfaces, output schemas, and enterprise self-service readiness checks without printing secret values.

Enterprise SSO release gate: current OpenSnow marketplace/BYOC examples are OIDC-only. Embedded native SAML has no metadata/ACS/assertion-validation path; configured SAML IdP login attempts must fail closed with `saml_unsupported_fail_closed`. Do not publish deployment, marketplace, or sales claims implying SAML is ready until a brokered or direct SAML profile lands with tests.

---

## 1. Local / On-Prem (Single Node)

The fastest path — zero dependencies, single binary, SQLite catalog.

### Install

```bash
# Option A (recommended today): Build from source
git clone https://github.com/opensnow/opensnow
cd opensnow
cargo build --release
cp target/release/opensnow /usr/local/bin/

# Option B: Docker Compose (local demo stack)
docker compose up --build opensnow

# Option C: Docker (HTTP only; no-auth is local/dev only). The container binds
# 0.0.0.0 internally to receive the forwarded port; the host publishes it on
# loopback only, and OPENSNOW_ALLOW_PUBLIC explicitly accepts the unauth listener.
docker build -t opensnow:local .
docker run \
  -e OPENSNOW_SERVER_HOST=0.0.0.0 \
  -e OPENSNOW_ALLOW_PUBLIC=1 \
  -p 127.0.0.1:8080:8080 \
  -v ~/.opensnow:/home/opensnow/.opensnow \
  opensnow:local
```

> Prebuilt distribution channels (a `curl | sh` installer, a published container
> image such as `ghcr.io/opensnow/opensnow`, a Homebrew tap, a Helm repository,
> and cloud-marketplace listings) are planned but **not yet published**. Until
> then, build from source or build the image locally as shown above.

### Start

```bash
# Zero-config start (auto-detects everything)
opensnow start

# With sample data
opensnow init --with-sample-data
opensnow start

# Web UI/API: http://localhost:8080
# pgwire is disabled by default. With auth disabled it is trusted-local smoke only.
# With OPENSNOW_JWT_SECRET/JWT mode set, psql password auth expects the OpenSnow bearer JWT.
```

Optional trusted-local pgwire smoke:

```bash
OPENSNOW_ENABLE_PGWIRE=1 opensnow start --enable-pgwire

docker run \
  -e OPENSNOW_ENABLE_PGWIRE=1 \
  -e OPENSNOW_SERVER_HOST=0.0.0.0 \
  -e OPENSNOW_ALLOW_PUBLIC=1 \
  -p 127.0.0.1:8080:8080 \
  -p 127.0.0.1:5433:5433 \
  -v ~/.opensnow:/home/opensnow/.opensnow \
  opensnow:local

# Docker Compose trusted-local pgwire override:
docker compose -f docker-compose.yml -f docker-compose.pgwire.yml up --build opensnow
```

Enterprise pgwire smoke after JWT auth is enabled:

```bash
export PGPASSWORD="$OPENSNOW_ACCESS_TOKEN"
psql -h 127.0.0.1 -p 5433 -U 'alice@example.com' -d 'acct_acme' -c 'SELECT * FROM allowed_orders'
```

The startup user must equal the JWT subject and the startup database must equal JWT `tenant_id`. pgwire then requires `sql.query` and `table.select`, evaluates the catalog-backed object policy before execution, records tenant query history, and appends audit allow/deny rows to the configured catalog.

### Public demo path

External testers should start with `scripts/demo.sh` or the smoke wrapper below. Local/dev no-auth mode is intentionally localhost-only; do not publish unauthenticated SQL endpoints on all interfaces. Docker examples bind host ports to `127.0.0.1` and use the non-root container home `/home/opensnow/.opensnow`.

For an OpenSnow-hosted tracked test instance, enable JWT auth and use evaluation sandbox registration instead of no-auth demo mode. `POST /api/v1/evaluation/register` issues a generated `eval-*` tenant with `EVALUATION` role, `sql.query table.select` scopes, demo SQL guardrails, and the `OPENSNOW_EVALUATION_QUERY_QUOTA` cap (default 100). Platform operators can audit usage with `GET /api/v1/evaluation/accounts` and suspend/revoke accounts with the admin-only `/api/v1/evaluation/accounts/{client_id}/suspend|revoke` endpoints. This sandbox is explicitly not enterprise BYOC mode; production customer data belongs in account-owned infrastructure.

### Self-service enterprise account lifecycle

Enterprise mode starts with durable account registration, not demo tenant headers. In marketplace/BYOC provisioning code paths, use the catalog `register_enterprise_account` gate with a durable entitlement carrying `account.activate`; missing, suspended, expired, cancelled, missing-feature, or mismatched organization identity fail closed before `ACCOUNTOWNER` bootstrap and append an audit deny event. The paired catalog `create_enterprise_warehouse` helper checks active `warehouse.activate` entitlement state, matching organization id, and per-account/org `warehouse_limit`, and records account/org ownership on the warehouse row. The SQL runtime `CREATE WAREHOUSE` path is still a launch gap for marketplace/BYOC self-service because it does not yet derive an authenticated account/org entitlement context before warehouse creation; do not use it as enterprise entitlement coverage. External customer/product/billing-owner reconciliation is handled by marketplace entitlement ingestion/reconciliation and is not a complete catalog-level activation claim yet. The legacy `POST /api/v1/accounts/register` local/demo path accepts `{"account_name":"Acme Corp","owner_email":"owner@acme.test"}` and writes the account root, default organization, default workspace, `ACCOUNTOWNER` membership, role mapping, and bootstrap service identity into the catalog. The response returns a `bootstrap` object containing those records; keep it server-side or deliver it through your provisioning workflow, never through the public demo UI.

Account-owned admin mutations use authenticated JWT context as the source of account scope. For example, `POST /api/v1/admin/accounts/{account_id}/workspaces` requires `policy.admin`, rejects spoofed `X-Tenant-ID` and `X-Account-ID` values in `jwt_required`, and denies cross-account path mutation unless the token has the explicit `break_glass.admin` scope. Customer account owners should receive tokens whose `tenant_id` matches their account id; platform break-glass tokens must be short-lived, audited, and reserved for recovery operations.

For local/operator administration against the configured catalog path, the CLI exposes the same lifecycle primitives without going through the public demo flow:

```bash
opensnow account-register --account-name "Acme Corp" --owner-email owner@acme.test
opensnow account-workspace-create --account-id acme-corp --name analytics
```

The evaluation sandbox is stored separately from enterprise accounts. Do not use `/api/v1/evaluation/register` identities for enterprise org/workspace provisioning, marketplace activation, customer-owned IdP setup, or production data access.

### Durable service clients / API keys

Enterprise service identities are catalog-backed and account/workspace scoped. Admins with `policy.admin` can create, list, rotate, and revoke service clients through `/api/v1/service-clients` and `/api/v1/service-clients/{client_id}/rotate|revoke`. Creation/rotation accepts or generates a raw `client_secret` and returns it only in that one response; persisted records store Argon2 hashes plus scopes, lifecycle status, expiry, rotation/revocation metadata, and `last_used_at` telemetry. List/read responses intentionally omit `secret_hash` and never replay raw secrets.

`/auth/token` resolves durable service clients from the configured catalog before falling back to `OPENSNOW_CLIENTS`. Keep `OPENSNOW_CLIENTS` for local/demo/bootstrap use only; it is not the enterprise source of truth. Durable clients issue JWTs with the registered account tenant, role, and scopes. Protected routes re-check durable client state on bearer-token use, so revoked/suspended/expired service clients fail closed even for already-issued tokens.

Example:

```bash
admin_access=$(curl -fsS -X POST http://127.0.0.1:8080/auth/token \
  -H 'Content-Type: application/json' \
  -d '{"grant_type":"client_credentials","client_id":"admin-client","client_secret":"REDACTED"}' | jq -r .access_token)

curl -fsS -X POST http://127.0.0.1:8080/api/v1/service-clients \
  -H "Authorization: Bearer ${admin_access}" \
  -H 'Content-Type: application/json' \
  -d '{"client_id":"svc-acme-loader","account_id":"acme-corp","workspace_id":"acme-corp-default","role":"ANALYST","scopes":["sql.query","table.select"]}'
```

Store the returned `client_secret` in the customer's secret manager immediately. Do not log it, put it in Helm values, or send it through public demo UI flows.

### Audit envelope and account-scoped export

OpenSnow now writes SQL query allow/deny/error activity, SCIM token lifecycle, SCIM user/group mutations, and marketplace entitlement ingestion into the shared enterprise audit envelope as well as legacy query history where applicable. Audit rows are catalog-backed in `audit_events`, use monotonic IDs, include actor/auth method/action/resource/result/request ID fields, and are protected by SQLite triggers plus app-level helpers that deny update/delete. Metadata is recursively redacted for token, authorization, secret, password, credential, access-key, and private-key fields before persistence.

Auditors use an account-scoped API protected by `audit.read` or `policy.admin`:

```bash
curl -fsS "http://127.0.0.1:8080/api/v1/admin/accounts/acme-corp/audit/events?action=sql.query&limit=100" \
  -H '<auth header with audit.read or policy.admin>' \
  | jq '.events[] | {id,event_time,account_id,action,result,request_id}'
```

The API rejects cross-account reads unless the caller carries explicit break-glass admin scope. Treat the current JSON response as the launch-baseline SIEM handoff; external export scheduling/retention and full emission coverage for every auth/SSO/secret/admin/deployment event remain P0 follow-up before broad enterprise claims.

### SCIM user and group lifecycle

Account admins can rotate account-scoped SCIM tokens for IdP provisioning:

```bash
admin_access=$(curl -fsS -X POST http://127.0.0.1:8080/auth/token \
  -H 'Content-Type: application/json' \
  -d '{"grant_type":"client_credentials","client_id":"admin-client","client_secret":"REDACTED"}' | jq -r .access_token)

scim_secret=$(curl -fsS -X POST http://127.0.0.1:8080/api/v1/admin/accounts/acme-corp/scim/tokens \
  -H '<auth header>' \
  -H 'Content-Type: application/json' \
  -d '{"label":"okta-prod"}' | jq -r .token.secret)
```

Give the raw SCIM secret to the IdP only once. When `OPENSNOW_AUTH_CATALOG_PATH` or `OPENSNOW_CATALOG_PATH` is configured, OpenSnow stores only a durable hash in account-scoped catalog metadata and supports restart-safe account-scoped listing via `GET /api/v1/admin/accounts/{account_id}/scim/tokens` and revocation via `DELETE /api/v1/admin/accounts/{account_id}/scim/tokens/{token_id}`. Token create/list/revoke admin routes require the JWT tenant/account to match the path account before state mutation; only short-lived platform break-glass tokens with the explicit `break_glass.admin` scope may administer another account, and token events are emitted to the shared append-only audit envelope with request metadata redacted. IdPs then call `/scim/v2/Users` and `/scim/v2/Groups` with `Content-Type: application/scim+json` and the SCIM bearer token. The implemented durable lifecycle covers user create/update/list/get, `PATCH active=false` deactivation, user DELETE deactivation, group create/update/list/get, group DELETE tombstones, userName filter basics, startIndex/count pagination, group-to-role metadata, account isolation, and shared audit events.

Deactivation immediately marks the SCIM user inactive, persists the lifecycle state, and attempts service-client revocation for a matching client id. Customer-facing production deployments must configure a catalog path; process-local SCIM state remains only for tests/local auth-disabled smoke paths and is not an enterprise-mode launch path.

```bash
scripts/demo.sh
scripts/quickstart-smoke.sh --mode local
scripts/quickstart-smoke.sh --mode docker
scripts/quickstart-smoke.sh --mode k3d
scripts/demo.sh reset
```

### Virtual warehouse controls

Local/single-node OpenSnow now has real catalog-backed warehouse routing and cost-control guardrails, but not Snowflake-equivalent elastic billing. Use SQL commands through `/api/v1/query` or compatible clients:

```sql
CREATE WAREHOUSE analytics WITH SIZE = 'small' MAX_NODES = 1;
ALTER WAREHOUSE analytics RESUME;
SHOW WAREHOUSES;
```

Then route HTTP queries with `{"warehouse":"analytics","sql":"SELECT 1"}`. Named warehouses reject data queries while `SUSPENDED`; resume before use or choose `default`. Query responses include `warehouse`, `warehouse_size`, `duration_ms`, and `warehouse_credits_estimate` so demos and hosted sandboxes can attribute usage. Prometheus exposes `opensnow_warehouse_pending_queries`, `opensnow_warehouse_queries_total`, and `opensnow_warehouse_credits_estimate_total` for dashboards/cost reports.

`CREATE WAREHOUSE` accepts only safe unquoted warehouse names (letters, numbers, underscores; cannot start with a number). `SIZE` must be one of `xsmall`, `small`, `medium`, `large`, or `xlarge`; `MIN_NODES`, `MAX_NODES`, and `AUTO_SUSPEND`/`AUTO_SUSPEND_SECONDS` must parse as non-negative integers, and `MAX_NODES` must be greater than or equal to `MIN_NODES`. Those node and suspend fields are stored/displayed metadata in the current local/single-node slice; they do not provision workers, enforce per-warehouse admission/routing limits, or auto-suspend compute yet.

Safety controls for demo/hosted environments:

```bash
# default: 4, hard cap: 64
OPENSNOW_MAX_CONCURRENT_QUERIES=4

# default: 30 seconds, hard cap: 300 seconds
OPENSNOW_QUERY_TIMEOUT_SECS=30

# bounds /api/v1/export/postgres target connect + load network I/O;
# default: 30 seconds, hard cap: 300 seconds
OPENSNOW_PG_EXPORT_TIMEOUT_SECS=30
```

Kubernetes-per-warehouse worker pools, KEDA autosuspend/auto-resume, and hard resource/billing isolation remain roadmap work. Current controls are admission, state gating, timeout, metrics, and usage estimation.

### Config (`opensnow.toml`)

The local binary can generate development credentials on first start. If you
copy this file for a hosted/cloud deployment, replace the placeholder with a
unique generated secret or inject `OPENSNOW_AUTH_ADMIN_PASSWORD` from your
secret manager. Do not publish the local demo auth mode outside localhost.

```toml
[server]
http_port = 8080
pg_port   = 5433
host      = "0.0.0.0"

[storage]
warehouse_path = "~/.opensnow/warehouse"   # local filesystem

[catalog]
path = "~/.opensnow/catalog.db"            # SQLite

[auth]
admin_password = "<generated-secret>"      # local demo only; use a secret for hosted/cloud deployments
```

### On-Prem with NFS / SAN storage

```toml
[storage]
warehouse_path = "/mnt/nfs/opensnow/warehouse"   # mount NFS here

[catalog]
# Use PostgreSQL for the catalog when running multi-node on-prem
database_url = "postgresql://opensnow:pass@pg-host:5432/opensnow"
```

---

## 2. On-Prem Kubernetes (Multi-Node)

Uses the Helm chart in `deploy/helm/opensnow/` and MinIO for S3-compatible object storage.

### Prerequisites

- Kubernetes 1.26+
- Helm 3.x
- `kubectl` configured
- Storage class with `ReadWriteOnce` PVCs

### Quick start with k3d (local testing)

```bash
# Start local K8s cluster
k3d cluster create --config deploy/k3d-config.yaml

# Deploy with MinIO (S3-compatible, included in chart)
helm upgrade --install opensnow deploy/helm/opensnow \
  -f deploy/helm/opensnow/values-dev.yaml \
  --set config.storage.type=s3 \
  --set config.storage.endpoint=http://opensnow-minio:9000 \
  --set worker.replicas=3

# Port-forward HTTP only by default; pgwire service exposure is disabled unless coordinator.pgwireEnabled=true.
kubectl port-forward svc/opensnow-gateway 8080:8080

# Trusted local pgwire compatibility smoke only:
# helm upgrade --install opensnow deploy/helm/opensnow -f deploy/helm/opensnow/values-dev.yaml --set coordinator.pgwireEnabled=true
# kubectl port-forward svc/opensnow-gateway 5433:5433
```

### Production on-prem K8s

```bash
# Use external PostgreSQL for catalog (recommended for prod)
helm upgrade --install opensnow deploy/helm/opensnow \
  --set config.storage.type=s3 \
  --set config.storage.endpoint=http://minio.storage.svc:9000 \
  --set config.storage.bucket=opensnow \
  --set config.storage.existingSecret=opensnow-storage \
  --set catalog.postgresUrl=postgresql://opensnow:pass@pg:5432/opensnow \
  --set worker.replicas=3 \
  --set keda.minReplicaCount=0 \
  --set keda.maxReplicaCount=20

# Apply KEDA autoscaling
kubectl apply -f deploy/keda-scaledobject.yaml
```

### Helm values reference

| Key | Default | Description |
|-----|---------|-------------|
| `worker.replicas` | `1` | Initial worker count |
| `keda.minReplicaCount` | `0` | Scale-to-zero minimum |
| `keda.maxReplicaCount` | `8` | Max workers |
| `config.storage.type` | `local` | `local` \| `minio` \| `s3` \| `gcs` \| `azure` |
| `catalog.postgresUrl` | `""` | Empty = SQLite |
| `auth.adminPassword` | `"OPEN/SNOW/DEMO/ONLY"` | Demo placeholder; use a Secret in production |
| `metadata.builtin.password` | `""` | Empty = reuse existing metadata Secret or generate a per-install password with Helm; set only from an explicit secret-management workflow |
| `tls.enabled` | `false` | Enable TLS termination |
| `coordinator.pgwireEnabled` | `false` | Expose PostgreSQL wire protocol service port; enable only for trusted local compatibility smoke, or with JWT auth configured so pgwire startup/session/policy/audit enforcement is active |
| `enterprise.sealedSecrets.enabled` | `false` | Required for enterprise/BYOC renders; local/test-instance can leave disabled |
| `enterprise.sealedSecrets.provider` | `""` | Enterprise secret boundary (`aws-secrets-manager`, `gcp-secret-manager`, or `vault`); never a raw secret value |
| `enterprise.sealedSecrets.kmsKeyArn` | `""` | KMS key ARN / cloud KMS key / Vault transit key reference used to seal or resolve secret handles |

---

## 3. AWS (EKS + S3 + RDS)

AWS Marketplace/BYOC remains gated until enterprise auth/security QA signs off. Use this package to install into a customer-owned AWS account after the constrained local/Docker/k3d demo path is safe; do not market it as a public enterprise launch path while the SSO/SCIM/RBAC/audit blockers in `docs/ENTERPRISE_AUTH_QA_VALIDATION.md` remain open.

### Terraform (automated)

```bash
cd deploy/terraform
terraform init
terraform validate
terraform apply \
  -var="deployment_mode=aws-marketplace" \
  -var="warehouse_bucket=my-org-opensnow-warehouse" \
  -var="audit_export_bucket=my-org-opensnow-audit" \
  -var="create_rds=true" \
  -var='marketplace_entitlement={product_code="prod-abc",customer_identifier="cust-123",entitlement_id="ent-789"}'
```

The module provisions private EKS worker nodes, an S3 warehouse bucket with versioning/SSE-KMS/public-access-block, an Object-Lock audit bucket, scoped IRSA, and optional private RDS PostgreSQL with managed master password, 7-day backups, deletion protection, and security-group ingress only from EKS nodes. Missing AWS credentials are an environment limitation: `terraform validate` can run locally, but `terraform plan/apply` require a configured customer AWS account.

### Helm install on EKS

No static AWS access keys are rendered into Helm. Use IRSA for S3/KMS access and an external-secret controller to sync the RDS managed password ARN from `terraform output -raw metadata_rds_master_secret_arn` into the Kubernetes Secret named by `metadata.external.existingSecret`. Enterprise renders fail closed if `config.storage.access_key` or `config.storage.secret_key` is set; object-store credentials must come from workload identity or from a sealed secret handle resolved only inside trusted runtime paths.

OpenSnow's auth crate now models the secret boundary with `SecretProviderConfig` values for AWS Secrets Manager/KMS, GCP Secret Manager/KMS, Vault transit/kv, and a local-dev sealed SQLite store. Runtime config parsing enforces `enterprise.secret_provider.enabled=true`, a provider in `aws-secrets-manager`, `gcp-secret-manager`, or `vault`, KMS/transit key metadata, and no inline object-store keys before enterprise/BYOC configs can start. Persist catalog/API records as handle IDs plus provider metadata only. Admin SSO setup can accept an external handle such as `aws-secretsmanager://arn:aws:secretsmanager:...:secret:opensnow/prod/oidc`, `gcp-secretmanager://projects/acme/secrets/opensnow-prod-oidc/versions/latest`, or `vault://kv/data/opensnow/prod/oidc#client_secret`; trusted runtime resolution goes through the provider boundary and fails closed if the AWS/GCP/Vault resolver is unavailable. Do not put IdP client secrets, object-store keys, catalog integration passwords, external-stage credentials, BI OAuth secrets, or marketplace/BYOC credentials in Helm values, Terraform variables, audit metadata, or API responses.

```bash
$(terraform output -raw kubeconfig_command)
helm upgrade --install opensnow ../helm/opensnow \
  --namespace opensnow --create-namespace \
  -f ../helm/opensnow/values-enterprise-aws.yaml \
  --set serviceAccount.annotations."eks\\.amazonaws\\.com/role-arn"="$(terraform output -raw irsa_role_arn)" \
  --set config.storage.bucket="$(terraform output -raw warehouse_bucket)" \
  --set enterprise.auditExport.bucket="$(terraform output -raw audit_export_bucket)"
```

Render and inspect before applying:

```bash
helm template opensnow ../helm/opensnow \
  -f ../helm/opensnow/values-enterprise-aws.yaml \
  --set serviceAccount.annotations."eks\\.amazonaws\\.com/role-arn"="arn:aws:iam::111122223333:role/acme-opensnow-irsa" \
  > /tmp/opensnow-aws-render.yaml
```

Confirm there are no inline cloud credentials, pgwire remains disabled unless explicitly enabled with TLS/source ranges, and all enterprise secrets reference existing Kubernetes/ExternalSecret-managed names. On QA hosts without the `helm` binary, run the static no-Helm guard instead:

```bash
python -m pytest tests/test_enterprise_secret_deployment_static.py -q
```

This checks the chart template carries the same runtime `enterprise.secret_provider` fail-closed guards and that AWS/GCP enterprise values use external secret handles/workload identity only.

### AWS/GCP secret-provider smoke inputs

Use provider handles in customer-owned secret managers before calling admin/catalog APIs. Examples:

```json
{
  "account_id": "acct_acme",
  "slug": "okta",
  "name": "Acme Okta",
  "protocol": "oidc",
  "oidc_issuer": "https://idp.acme.example/",
  "oidc_client_id": "opensnow",
  "oidc_client_secret_handle": "aws-secretsmanager://arn:aws:secretsmanager:us-east-1:111122223333:secret:opensnow/prod/oidc"
}
```

```json
{
  "account_id": "acct_acme",
  "slug": "okta",
  "name": "Acme Okta",
  "protocol": "oidc",
  "oidc_issuer": "https://idp.acme.example/",
  "oidc_client_id": "opensnow",
  "oidc_client_secret_handle": "gcp-secretmanager://projects/acme-prod/secrets/opensnow-prod-oidc/versions/latest"
}
```

```json
{
  "account_id": "acct_acme",
  "slug": "okta",
  "name": "Acme Okta",
  "protocol": "oidc",
  "oidc_issuer": "https://idp.acme.example/",
  "oidc_client_id": "opensnow",
  "oidc_client_secret_handle": "vault://kv/data/opensnow/prod/oidc#client_secret"
}
```

AWS runtime resolution uses the AWS Secrets Manager trusted execution path, GCP uses Secret Manager through the configured workload identity/application credentials path, and Vault uses the configured Vault session. If any resolver dependency or IAM/Vault policy is missing, startup/login fails closed instead of falling back to plaintext.

### What Terraform provisions

| Component | Service / hardening |
|---|---|
| Kubernetes | EKS managed node group in private subnets |
| Object storage | S3 warehouse bucket, versioning, SSE-KMS, block public access |
| Audit export | Separate S3 bucket, versioning, Object Lock Governance retention, SSE-KMS |
| Catalog DB | Optional private RDS PostgreSQL, managed password, backups, deletion protection |
| Pod identity | IRSA scoped to `system:serviceaccount:<namespace>:<service_account>` |
| IAM | S3 warehouse read/write, audit append, KMS use; no static keys |
| Network exposure | Helm default `ClusterIP`; AWS example uses NLB only with TLS annotation and source ranges |

### SBOM and image provenance

Before marketplace submission, produce and publish image provenance alongside the chart:

```bash
OPENSNOW_IMAGE_TAG=${OPENSNOW_IMAGE_TAG:-latest}
OPENSNOW_IMAGE="opensnow/opensnow:${OPENSNOW_IMAGE_TAG}"

syft "${OPENSNOW_IMAGE}" -o spdx-json > sbom.spdx.json
cosign verify --certificate-identity-regexp '.*' --certificate-oidc-issuer-regexp '.*' "${OPENSNOW_IMAGE}"
cosign attest --predicate sbom.spdx.json --type spdxjson "${OPENSNOW_IMAGE}"
```

Keep the digest pinned in marketplace artifacts. Do not submit to AWS Marketplace without human approval and QA signoff.

### Upgrade / rollback / backup / restore / uninstall

- Upgrade: save `helm get values`, render with `helm template`, then use `helm upgrade --install --atomic --timeout 10m`.
- Rollback: `helm rollback opensnow <REVISION> -n opensnow`; verify `/health` and a read-only sample query before resuming writes.
- Backup: use S3 versioning/replication for warehouse data, RDS automated backups/PITR for metadata, and Object-Lock audit export retention. Catalog and Parquet data must be restored together.
- Restore: restore RDS to a new endpoint, sync its password to the expected Secret, update `metadata.external.host`, and run a Helm upgrade; restore/sync S3 warehouse prefixes before starting workers.
- Uninstall: `helm uninstall opensnow -n opensnow`; only then consider `terraform destroy`. Buckets and RDS deletion protection intentionally prevent accidental data loss.

### AWS Marketplace package status

The current repository contains an AWS-first BYOC reference package, not a submitted public listing. Marketplace activation values (`enterprise.marketplace.*`) provide deployment inputs, while runtime entitlement events are ingested through `POST /api/v1/marketplace/aws/entitlements` with `x-opensnow-marketplace-signature` backed by `OPENSNOW_MARKETPLACE_WEBHOOK_SECRET`. The payload shape is fixture-driven; see `tests/fixtures/aws-marketplace-entitlement-active.json` and `tests/fixtures/aws-marketplace-entitlement-expired.json`.

AWS entitlement ingestion persists provider/entitlement/account/organization/customer/product/plan/state/features/warehouse-limit/billing-owner metadata and emits append-only audit action `marketplace.entitlement.ingest`. Catalog-level enterprise warehouse provisioning through `create_enterprise_warehouse` enforces suspended, expired, cancelled, missing `warehouse.activate`, and purchased warehouse-limit denial cases, but the SQL runtime `CREATE WAREHOUSE` command is not yet wired to authenticated marketplace entitlement context. It does not replace OpenSnow authorization, SQL privileges, SSO/OIDC/SAML, SCIM, or audit controls.

GCP and Azure marketplace entitlement ingestion are not supported yet. Do not claim GCP/Azure marketplace support until provider-specific fixtures, signature validation, lifecycle tests, and cloud billing reconciliation paths exist.

---

## 4. GCP (GKE + GCS + Cloud SQL)

### Terraform

```bash
cd deploy/terraform/gcp
cp example.tfvars terraform.tfvars
```

Edit `terraform.tfvars`:
```hcl
project      = "my-gcp-project"
region       = "us-central1"
cluster_name = "opensnow-prod"
gcs_bucket   = "my-opensnow-warehouse"
db_password  = "<generated-db-password>"
```

```bash
terraform init
terraform apply
```

### What Terraform provisions

| Component | Service |
|---|---|
| Kubernetes | GKE Autopilot |
| Object Storage | GCS bucket |
| Catalog DB | Cloud SQL PostgreSQL |
| Load Balancer | Cloud Load Balancing |
| Pod Identity | Workload Identity |
| Secrets | Secret Manager |

### Manual Helm deploy on existing GKE

Use `deploy/helm/opensnow/values-enterprise-gcp.yaml` for customer-owned GKE/GCS/Cloud SQL deployments. It sets `enterprise.sealedSecrets.provider="gcp-secret-manager"`, disables built-in metadata, keeps pgwire closed by default, and expects GKE Workload Identity rather than inline JSON keys.

```bash
# Authenticate GCS via Workload Identity (recommended)
helm upgrade --install opensnow deploy/helm/opensnow \
  --namespace opensnow --create-namespace \
  -f deploy/helm/opensnow/values-enterprise-gcp.yaml \
  --set config.storage.bucket=my-opensnow-warehouse \
  --set config.storage.gcs_project_id=my-gcp-project \
  --set metadata.external.host="10.10.0.4" \
  --set metadata.external.existingSecret="opensnow-cloudsql"
```

On hosts without Helm, use the static chart/value smoke:

```bash
python -m pytest tests/test_enterprise_secret_deployment_static.py -q
```

GCP IdP/client secrets should be created in Secret Manager and synchronized into Kubernetes or represented as provider handles. Do not set `config.storage.gcs_service_account_path` in enterprise mode; runtime config validation treats explicit key files as inline credentials and fails closed.

### GCP Marketplace

1. Go to: https://console.cloud.google.com/marketplace/product/opensnow
2. Click **Configure**
3. Choose cluster, region, GCS bucket
4. Click **Deploy**

---

## 5. Azure (AKS + Blob + PostgreSQL)

### Terraform

```bash
cd deploy/terraform/azure  # TODO: Azure Terraform is not checked in yet
cp example.tfvars terraform.tfvars
```

Edit `terraform.tfvars`:
```hcl
resource_group  = "opensnow-rg"
location        = "eastus"
cluster_name    = "opensnow-prod"
storage_account = "myopensnowstorage"
db_password     = "<generated-db-password>"
```

```bash
terraform init
terraform apply
```

### What Terraform provisions

| Component | Service |
|---|---|
| Kubernetes | AKS |
| Object Storage | Azure Blob Storage |
| Catalog DB | PostgreSQL Flexible Server |
| Load Balancer | Azure Load Balancer |
| Pod Identity | Managed Identity (OIDC) |
| Secrets | Azure Key Vault |

### Manual Helm deploy on existing AKS

```bash
helm upgrade --install opensnow deploy/helm/opensnow \
  --set config.storage.type=azure \
  --set config.storage.azure_container=opensnow \
  --set config.storage.azure_account_name=myopensnowstorage \
  --set catalog.postgresUrl="postgresql://opensnow:pass@pg-host:5432/opensnow"
```

### Azure Marketplace

1. Go to: https://azuremarketplace.microsoft.com/en-us/marketplace/apps/opensnow
2. Click **Get It Now** → **Create**
3. Fill in resource group, region, worker count
4. Click **Review + create** → **Create**

---

## 6. Configuration Reference

Full `opensnow.toml` with all options:

```toml
[server]
http_port  = 8080          # REST API + Web UI
pg_enabled = false         # pgwire is disabled by default; enable only for trusted local/client-compatibility smoke
pg_port    = 5433          # PostgreSQL wire protocol port when pg_enabled/OPENSNOW_ENABLE_PGWIRE is enabled
flight_port = 32010        # Arrow Flight SQL
host       = "0.0.0.0"
tls_cert   = ""            # path to TLS cert (leave empty to disable)
tls_key    = ""

[storage]
# Local filesystem (dev/single-node)
warehouse_path = "~/.opensnow/warehouse"

# S3 / MinIO
# type          = "s3"
# s3_endpoint   = "http://minio:9000"    # omit for real AWS S3
# s3_bucket     = "opensnow"
# s3_region     = "us-east-1"
# s3_access_key = ""                     # leave empty for IRSA / instance role
# s3_secret_key = ""

# GCS
# type          = "gcs"
# gcs_bucket    = "opensnow"
# gcs_project   = "my-project"

# Azure Blob
# type               = "azure"
# azure_container    = "opensnow"
# azure_account_name = "mystorageaccount"

[catalog]
# SQLite (dev / single-node)
path = "~/.opensnow/catalog.db"

# PostgreSQL (production / multi-node)
# database_url = "postgresql://user:***@host:5432/opensnow"

[auth]
admin_password    = "<generated-secret>"   # set from a generated secret, not a shared default
jwt_secret        = "<generated-secret>"   # local/dev HS256 only; do not use for enterprise/BYOC
jwt_expiry_hours  = 24

# Enterprise/BYOC product-token issuer (preferred for production):
# OPENSNOW_JWT_MODE=enterprise
# OPENSNOW_JWT_ALGORITHM=RS256                 # ES256 also supported
# OPENSNOW_JWT_ISSUER=https://opensnow.example/auth
# OPENSNOW_JWT_AUDIENCE=opensnow-api
# OPENSNOW_JWT_KID=opensnow-prod-2026-05
# OPENSNOW_JWT_PRIVATE_KEY_PEM / OPENSNOW_JWT_PRIVATE_KEY_PATH
# OPENSNOW_JWT_PUBLIC_KEY_PEM  / OPENSNOW_JWT_PUBLIC_KEY_PATH
# OPENSNOW_JWT_JWK_N and OPENSNOW_JWT_JWK_E publish /.well-known/jwks.json
# OPENSNOW_JWT_VERIFICATION_KEYS_JSON carries old verify-only keys during rotation
# OPENSNOW_JWT_REVOKED_KIDS is a comma-separated fail-closed deny-list

[rapids]
enabled            = false    # enable GPU acceleration (requires NVIDIA GPU + cuDF)
python_bin         = "python3"
gpu_memory_limit_mb = 4096
fallback_to_cpu    = true     # always true — safe default
```

### Environment variable overrides

Every config key can be overridden with `OPENSNOW_` prefix + uppercased key:

```bash
export OPENSNOW_AUTH_ADMIN_PASSWORD=$(openssl rand -base64 32)
export OPENSNOW_AUTH_JWT_SECRET=$(openssl rand -base64 32)
OPENSNOW_ENABLE_PGWIRE=1              # trusted local/client-compatibility smoke only
OPENSNOW_SERVER_PG_PORT=5433
OPENSNOW_STORAGE_S3_BUCKET=my-bucket
OPENSNOW_CATALOG_DATABASE_URL=postgresql://...
```


### Cloud runtime safety notes

- Local demo examples are for localhost-only trials. Hosted/cloud deployments
  must use unique generated secrets, OIDC/SSO where available, TLS or private
  endpoints, and Kubernetes/cloud secret-manager injection rather than
  copy/pasteable passwords in config files.
- For Kubernetes, prefer an existing-secret path: create a Secret or External
  Secret containing the admin password/JWT/storage credentials, then reference
  it with chart values such as `config.storage.existingSecret`. Enterprise mode
  must use `enterprise.jwt.mode=enterprise` (RS256/ES256), a non-empty issuer,
  audience, `kid`, and an existing secret containing the active private/public
  key pair plus public JWK coordinates. The chart renders those into
  `OPENSNOW_JWT_*` env vars and exposes `/auth/jwks.json` and
  `/.well-known/jwks.json`; local/dev HS256 remains available only through
  `OPENSNOW_JWT_SECRET`/`local_hs256` for localhost testing.
- Do not render object-store credentials into Helm ConfigMaps. Use `OPENSNOW_STORAGE_ACCESS_KEY` and `OPENSNOW_STORAGE_SECRET_KEY` only from Kubernetes Secrets such as `existingSecret`, or prefer AWS IRSA / GCP Workload Identity so no static keys are present.
- Keep `OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP` disabled outside local MinIO demos.
- Restrict pgwire and admin endpoints with `loadBalancerSourceRanges` before sharing any hosted demo.
- Validate rollout and recovery with `kubectl rollout status deploy/opensnow-coordinator`, `scripts/public-smoke.sh`, `scripts/quickstart-smoke.sh --mode local`, and `helm rollback opensnow` drills.
- Run a secret scan before publishing images, Helm values, Terraform variables, or demo bundles.

```bash
kubectl create namespace opensnow
kubectl -n opensnow create secret generic opensnow-storage \
  --from-literal=access-key='REDACTED_ACCESS_KEY' \
  --from-literal=secret-key='REDACTED_SECRET_KEY'

helm upgrade --install opensnow deploy/helm/opensnow \
  --namespace opensnow \
  --set config.storage.type=s3 \
  --set config.storage.bucket=my-opensnow-warehouse \
  --set config.storage.region=us-east-1 \
  --set config.storage.existingSecret=opensnow-storage \
  --set gateway.type=ClusterIP

helm upgrade --install opensnow deploy/helm/opensnow \
  --namespace opensnow \
  --set gateway.type=LoadBalancer \
  --set-json 'gateway.loadBalancerSourceRanges=["203.0.113.10/32"]'

kubectl -n opensnow rollout status deploy/opensnow-coordinator
kubectl -n opensnow get pods,svc
kubectl -n opensnow port-forward svc/opensnow-gateway 8080:8080
# Optional trusted pgwire smoke only after installing with --set coordinator.pgwireEnabled=true:
# kubectl -n opensnow port-forward svc/opensnow-gateway 5433:5433
scripts/public-smoke.sh
scripts/quickstart-smoke.sh --mode local
scripts/quickstart-smoke.sh --mode docker
scripts/quickstart-smoke.sh --mode k3d
helm -n opensnow rollback opensnow

git grep -nE '[m]inioadmin|admin_password\s*=\s*"admin"|GF_SECURITY_ADMIN_PASSWORD:\s*admin|s3_(access|secret)_key\s*=\s*"[^"#]+' -- . ':!target' ':!docs/ENTERPRISE_AUTH_QA_VALIDATION.md'
```

---

## 7. Database Setup

### SQLite (automatic)

Tables are created automatically on first start. Nothing to do.

### PostgreSQL (production)

```bash
# Create database
createdb opensnow

# Apply migrations in order
psql $DATABASE_URL -f db/migrations/000_core.sql
psql $DATABASE_URL -f crates/opensnow-auth/migrations/001_sso.sql

# Verify
psql $DATABASE_URL -c "\dt"
```

See `db/README.md` for full schema documentation.

---

## 8. SSO / OIDC Setup

SSO lets enterprise users log in with Google, Okta, Azure AD, Keycloak, or any OIDC-compatible IdP.

### Step 1: Create a tenant

```sql
INSERT INTO tenants (id, slug, name, sso_enabled, oidc_issuer, oidc_client_id, oidc_client_secret, allowed_domains)
VALUES (
  gen_random_uuid(),
  'acme',
  'Acme Corp',
  true,
  'https://accounts.google.com',          -- or your Okta/Azure issuer
  'your-client-id',
  'your-client-secret',
  '["acme.com", "acme.io"]'              -- users with these email domains auto-routed to SSO
);
```

Or via the admin API:
```bash
curl -X POST http://localhost:8080/api/v1/admin/tenants \
  -H "Authorization: Bearer <admin-access-token>" \
  -H "content-type: application/json" \
  -d '{"slug":"acme","name":"Acme Corp","sso_enabled":true,"oidc_issuer":"https://accounts.google.com","allowed_domains":["acme.com"]}'
```

### Step 2: Map IdP groups → OpenSnow roles

```bash
curl -X POST http://localhost:8080/api/v1/admin/tenants/{tenant_id}/sso-mappings \
  -H "Authorization: Bearer <admin-access-token>" \
  -H "content-type: application/json" \
  -d '{"idp_claim_key":"groups","idp_claim_value":"data-engineers","role_id":"SYSADMIN"}'
```

The SSO login route is `POST /api/v1/auth/sso/login`; configured OIDC domains return an authorization URL with state/nonce/PKCE. The callback route is `GET /api/v1/auth/sso/callback`; it exchanges the authorization code, verifies the IdP token with issuer/audience/nonce/email-domain/email_verified checks, persists an `sso_sessions` row in `OPENSNOW_SSO_DB_PATH`, and mints a scoped OpenSnow product token. Protected REST middleware re-opens that same SSO DB and rejects OIDC-derived product tokens when the session is missing, expired, revoked, or account/email mismatched. `OPENSNOW_JWT_SECRET` is still required for the short-lived product token signing key; service-client tokens remain HS256 until the asymmetric product-token issuer/JWKS slice lands.

Embedded SAML remains fail-closed: SAML IdP connections return `saml_unsupported_fail_closed` until a brokered metadata/ACS profile is implemented and documented.

### Step 3: Configure your IdP

| Provider | Issuer URL |
|---|---|
| Google | `https://accounts.google.com` |
| Okta | `https://{your-domain}.okta.com` |
| Azure AD | `https://login.microsoftonline.com/{tenant-id}/v2.0` |
| Keycloak | `https://{host}/realms/{realm}` |

Set the redirect URI to: `https://your-opensnow-host/api/v1/auth/sso/callback`

See `crates/opensnow-auth/src/sso.rs` for full OIDC implementation details.

---

## 9. Connecting Clients

The default public/local quickstart client path is HTTP (`http://localhost:8080`). PostgreSQL wire compatibility is disabled by default; use the examples below only for trusted local deployments where you explicitly started OpenSnow with `--enable-pgwire`, `OPENSNOW_ENABLE_PGWIRE=1`, or `[server].pg_enabled = true`, and bound port 5433 to localhost or another trusted private network. By default pgwire still enforces the public-demo safe SQL gate; set `OPENSNOW_TRUSTED_SQL=1` only on trusted/local deployments when tools such as dbt need full DDL and session-control compatibility.

Trusted-local pgwire examples after explicit opt-in:

```bash
# psql
psql -h localhost -p 5433 -U admin -d opensnow
```

```python
# Python (psycopg2) — connect over the PostgreSQL wire protocol
import os
import psycopg2
conn = psycopg2.connect(
    "host=localhost port=5433 user=admin dbname=opensnow",
    password=os.environ["OPENSNOW_PG_PASSWORD"],
)

# A native `opensnow` Python SDK is planned but not yet published; use psycopg2
# (or any PostgreSQL client) against the pgwire endpoint today.
```

```bash
# dbt needs both pgwire and trusted SQL enabled so table materializations can
# issue CREATE TABLE AS / DROP TABLE and dbt session-control statements.
OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_TRUSTED_SQL=1 opensnow start --enable-pgwire
```

```yaml
# dbt profiles.yml for trusted-local pgwire + trusted SQL opt-in only:
opensnow:
  target: dev
  outputs:
    dev:
      type: opensnow
      host: localhost
      port: 5433
      user: admin
      password: "{{ env_var('OPENSNOW_PG_PASSWORD') }}"
      dbname: opensnow
      schema: public
```

Tableau / Looker / Metabase can use their PostgreSQL connector on port 5433 only against trusted, explicitly pgwire-enabled deployments. Do not expose pgwire as a public/default endpoint until auth, tenant, RBAC/SQL-policy, and audit boundaries are enforced on that path.

### REST data movement endpoints

The REST API remains the default client path for local/public smoke. Two operator data-movement endpoints are available for trusted deployments:

```bash
# Register a local/object-store Parquet file or directory as a queryable table.
curl -X POST http://localhost:8080/api/v1/tables/register \
  -H "Authorization: Bearer ***" \
  -H "content-type: application/json" \
  -d '{"name":"orders_ext","uri":"s3://my-bucket/path/to/orders/"}'

# Export a validated OpenSnow query result into an external PostgreSQL table.
curl -X POST http://localhost:8080/api/v1/export/postgres \
  -H "Authorization: Bearer ***" \
  -H "content-type: application/json" \
  -d '{"sql":"SELECT * FROM orders_ext LIMIT 1000","dsn":"postgres://user:***@postgres.example.com:5432/app","schema":"public","table":"orders_ext","mode":"replace"}'
```

When auth is enabled these routes require the `policy.admin` scope. In auth-disabled local demo mode they are still reachable, so keep the HTTP listener bound to loopback/trusted private networks and do not expose them as public unauthenticated endpoints.

### Arrow Flight SQL (bulk transfer)

```python
from pyarrow import flight
client = flight.connect("grpc://localhost:32010")
info = client.get_flight_info(flight.FlightDescriptor.for_command(b"SELECT * FROM my_table"))
```

---

## 10. Monitoring

### Prometheus metrics

Exposed at `http://localhost:8080/metrics` (Prometheus format).

Key metrics:
- `opensnow_queries_total` — query count by status
- `opensnow_query_duration_seconds` — query latency histogram
- `opensnow_active_warehouses` — active virtual warehouses
- `opensnow_cache_hit_ratio` — L1/L2 cache effectiveness

### Grafana dashboard

```bash
# Import pre-built dashboard
kubectl apply -f deploy/grafana/

# Or import manually: deploy/grafana/opensnow-dashboard.json
```

### Health check

```bash
curl http://localhost:8080/health
# {"status":"ok","engine":"opensnow"}
```

---

## 11. Private Cloud Connectivity (Storage Performance)

This is one of OpenSnow's biggest cloud performance advantages.
Workers read/write Parquet files from object storage on every query — the network path matters a lot.

### The problem without private endpoints

```
EKS worker → NAT Gateway → public internet → S3
         ↑                      ↑
   bottleneck              egress cost
   (bandwidth-limited)     ($0.09/GB)
```

### With VPC endpoints (what we configure)

```
EKS worker → VPC Gateway Endpoint → S3  (private AWS backbone)
                    ↑
           FREE, no NAT, no internet, no egress cost
```

### AWS — S3 Gateway Endpoint

Planned for the AWS Terraform path under `deploy/terraform`; verify the VPC endpoint resource exists in the current plan before making marketplace/private-backbone claims.
This is **free** (AWS Gateway endpoints have no hourly or data transfer charge).

```hcl
# Automatically enabled — no config needed
# Routes all S3 traffic from EKS private subnets directly to S3
resource "aws_vpc_endpoint" "s3" {
  vpc_endpoint_type = "Gateway"
  service_name      = "com.amazonaws.${region}.s3"
  route_table_ids   = module.vpc.private_route_table_ids
}
```

**Result:** S3 reads go from ~5ms (via NAT) to ~1ms (private). Zero egress cost.

Optional interface endpoints (small hourly cost, enable if needed):
```bash
# Enable ECR (faster container pulls) and Secrets Manager
terraform apply \
  -var="enable_ecr_endpoints=true" \
  -var="enable_secrets_endpoint=true"
```

### GCP — Private Google Access

GKE Autopilot nodes automatically use **Private Google Access** — GCS traffic stays on Google's internal network. Nothing to configure.

For explicit control:
```hcl
# In gcp/terraform/main.tf — already enabled
google_compute_subnetwork {
  private_ip_google_access = true   # GCS traffic stays private
}
```

### Azure — Private Endpoints for Blob Storage

```hcl
# Add to azure/terraform/main.tf
resource "azurerm_private_endpoint" "storage" {
  name                = "${var.cluster_name}-storage-pe"
  resource_group_name = azurerm_resource_group.main.name
  location            = var.location
  subnet_id           = azurerm_subnet.private.id

  private_service_connection {
    name                           = "opensnow-blob"
    private_connection_resource_id = azurerm_storage_account.data.id
    subresource_names              = ["blob"]
    is_manual_connection           = false
  }
}
```

### Cost + Latency comparison

| Path | Latency | Cost |
|---|---|---|
| No endpoint (NAT → internet) | ~5ms | $0.09/GB egress |
| AWS S3 Gateway Endpoint | ~1ms | **Free** |
| GCP Private Google Access | ~1ms | Free |
| Azure Private Endpoint | ~1ms | ~$0.01/hr + $0.01/GB |

For a 3-node OpenSnow cluster querying 1TB/day, the S3 Gateway endpoint alone saves ~**$2,700/month** in NAT + egress costs.

### On-prem private connectivity

If your data is in on-prem storage (NFS, Ceph, MinIO) and compute is in cloud:
- Use **AWS Direct Connect** / **GCP Interconnect** / **Azure ExpressRoute**
- Or deploy OpenSnow fully on-prem — MinIO handles local S3-compatible storage with no cloud dependency

```toml
# opensnow.toml — on-prem with local MinIO
[storage]
type         = "s3"
s3_endpoint  = "http://minio.internal:9000"  # private, never touches internet
s3_bucket    = "opensnow"
s3_access_key = "..."
s3_secret_key = "..."
```

---

## Quick Reference

| Target | Time to deploy | Command |
|---|---|---|
| Local (binary) | < 2 min | `opensnow start` |
| Local (Docker) | < 2 min | `docker run opensnow/opensnow` |
| On-prem K8s | ~10 min | `helm install opensnow deploy/helm/opensnow` |
| AWS | ~15 min | `terraform apply` in `deploy/terraform` |
| GCP | ~15 min | `terraform apply` in `deploy/terraform/gcp` |
| Azure | TODO | Azure Terraform packaging is not checked in yet |
| AWS Marketplace | ~10 min | One-click via console |
| GCP Marketplace | ~10 min | One-click via console |
| Azure Marketplace | ~10 min | One-click via console |

Same binary and same SQL surface everywhere; HTTP is the default client path, and pgwire remains an explicit opt-in. Auth-disabled pgwire is loopback/trusted-local only. Auth-enabled pgwire can be used for controlled enterprise smoke when the PostgreSQL password is an OpenSnow bearer JWT and the connection is protected by loopback/port-forward or external TLS plus source-range controls.


### PostgreSQL wire public-demo gate

`[server].pg_enabled = false` is the default in local, Helm, and public-demo config. Auth-disabled pgwire must stay bound to loopback/trusted-local smoke because it has no tenant-isolation boundary. When JWT/enterprise auth is configured, pgwire authenticates with PostgreSQL CleartextPassword carrying the OpenSnow bearer JWT, rejects startup `user` values that do not match the JWT subject, rejects startup `dbname` values that do not match `tenant_id`, and runs scope/object-policy/audit checks before execution. The server still does not terminate TLS or implement SCRAM-SHA-256, so shared/hosted/marketplace deployments must keep pgwire disabled unless an external boundary provides TLS termination, source-range restriction, and secret-safe operational handling for `PGPASSWORD` bearer tokens.


## Enterprise BYOC / marketplace deployment

Enterprise deployment is Option B: OpenSnow runs in a customer-owned AWS account
or customer-controlled Kubernetes cluster, not as an unsafe shared public demo.
The AWS-first path provisions EKS, S3 warehouse storage, optional RDS metadata,
KMS encryption, audit-export storage, and IRSA in the customer-owned AWS account.
AWS Marketplace mode adds buyer/product/entitlement identity that must gate
account activation and warehouse activation; it does not replace OpenSnow
SQL/RBAC authorization.

OpenSnow test-instance mode is the default for local/dev/public evaluation. In
that mode Helm keeps `enterprise.mode="test-instance"`, pgwire is disabled by
default, local MinIO/built-in metadata may be used, and no enterprise SSO, SCIM,
audit-export, sealed-secret, or marketplace readiness claims should be made.

Production enterprise rendering must use:

```bash
terraform -chdir=deploy/terraform init -backend=false
terraform -chdir=deploy/terraform validate
helm lint deploy/helm/opensnow
helm template opensnow deploy/helm/opensnow \
  --namespace opensnow \
  -f deploy/helm/opensnow/values-enterprise-aws.yaml > /tmp/opensnow-enterprise.yaml
```

Required enterprise Helm values include external metadata secrets, customer IdP
OIDC inputs, SCIM token secret references, audit export bucket/KMS settings,
sealed secret/KMS provider settings, TLS, marketplace entitlement identity, and
restricted pgwire exposure. No SAML inputs are release-supported until a
brokered/direct SAML profile ships and `saml_unsupported_fail_closed` is removed
by tests. The chart fails rendering for non-test-instance mode when external
metadata secrets are not configured or required entitlement IDs are missing.
