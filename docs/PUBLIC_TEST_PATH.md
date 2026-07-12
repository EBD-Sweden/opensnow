# Public test path

This is the fastest safe path for an external user to try OpenSnow without private credentials or deployment help. It validates the local binary, Docker demo, k3d/Helm demo, sample data, first SQL query, REST API, health/status, and the current enterprise-auth readiness boundary. PostgreSQL wire is documented as an explicit opt-in: auth-disabled pgwire is loopback/trusted-local only, while auth-enabled enterprise pgwire requires bearer-JWT password auth plus loopback/port-forward or external TLS/source-range controls.

Current status: local quickstart is the recommended public test path. Kubernetes and cloud demo paths are documented, but marketplace-style production launch is blocked until the enterprise auth/security gaps in `docs/ENTERPRISE_AUTH_QA_VALIDATION.md` are closed.

## 0. Prerequisites

Local binary path:

- Rust 1.88+ with `cargo`
- `curl`
- Optional: `psql` plus `psycopg` or `psycopg2` for explicit trusted-local PostgreSQL wire smoke

Container path:

- Docker with Compose v2

Kubernetes path:

- Docker
- `k3d`
- `kubectl`
- Helm 3.x

## 1. One-command public demo

From a fresh clone, run the safest local/demo path:

```bash
git clone https://github.com/opensnow/opensnow.git
cd opensnow
scripts/demo.sh
```

Expected result:

- OpenSnow is running on `http://localhost:8080` unless it was already healthy there.
- Deterministic synthetic data from `demo/public-demo-manifest.json` is loaded through `scripts/demo-seed.py`.
- `scripts/public-smoke.sh` skips pgwire by default so unauthenticated pgwire is not exposed in the public demo path.
- Demo state is isolated under `.opensnow-demo/`.

Reset and cleanup:

```bash
scripts/demo.sh reset
```

Full one-command demo details: `docs/PUBLIC_DEMO.md`.

## 1.1 Hosted evaluation sandbox account mode

A platform-hosted OpenSnow test instance may enable JWT auth and expose an evaluation-only registration endpoint for external testers. This mode is not enterprise account/BYOC mode; it is an OpenSnow-owned sandbox with tracked credentials, sample data, demo SQL guardrails, and operator revocation.

Tester registration:

```bash
curl -sS -X POST http://localhost:8080/api/v1/evaluation/register \
  -H 'content-type: application/json' \
  -d '{"email":"tester@example.com"}'
```

The response returns one-time sandbox `client_id`, `client_secret`, `tenant_id`, `query_quota`, `token_endpoint`, and the explicit `evaluation_sandbox` mode. Use those credentials with `/auth/token`, then call protected REST endpoints with `Authorization: Bearer ${OPENSNOW_ADMIN_TOKEN:?set OPENSNOW_ADMIN_TOKEN}`. Each evaluation account gets its own generated `eval-*` tenant and is blocked from spoofing another tenant with `X-Tenant-ID`.

Operator controls require a `policy.admin`/platform-admin bearer token exported as `OPENSNOW_ADMIN_TOKEN`:

```bash
curl -sS http://localhost:8080/api/v1/evaluation/accounts \
  -H "authorization: Bearer ${OPENSNOW_ADMIN_TOKEN:?set OPENSNOW_ADMIN_TOKEN}"

curl -sS -X POST http://localhost:8080/api/v1/evaluation/accounts/$CLIENT_ID/suspend \
  -H "authorization: Bearer ${OPENSNOW_ADMIN_TOKEN:?set OPENSNOW_ADMIN_TOKEN}" \
  -H 'content-type: application/json' \
  -d '{"reason":"quota abuse or evaluation expired"}'

curl -sS -X POST http://localhost:8080/api/v1/evaluation/accounts/$CLIENT_ID/revoke \
  -H "authorization: Bearer ${OPENSNOW_ADMIN_TOKEN:?set OPENSNOW_ADMIN_TOKEN}" \
  -H 'content-type: application/json' \
  -d '{"reason":"customer upgraded to enterprise BYOC"}'
```

Sandbox boundaries:

- Evaluation accounts are registered as `kind=evaluation`, role `EVALUATION`, scopes `sql.query table.select`, and generated `eval-*` tenants.
- Every bearer request is checked against the live registry, so suspension/revocation invalidates previously issued tokens.
- Query quota is enforced for evaluation SQL tokens (`OPENSNOW_EVALUATION_QUERY_QUOTA`, default 100) and usage is visible in the operator account list.
- Demo/sample data uses `opensnow_demo_*` naming. Do not load customer production data into the sandbox; upgrade to enterprise BYOC/customer-owned infrastructure first.

## 2. Local quickstart

From a fresh clone:

```bash
git clone https://github.com/opensnow/opensnow.git
cd opensnow
cargo run -p opensnow-cli -- init --with-sample-data --industry both
cargo run -p opensnow-cli -- local "SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type"
cargo run -p opensnow-cli -- start
```

Expected local artifacts:

- `opensnow.toml` in the repo root if no config existed
- `~/.opensnow/warehouse/opensnow/public/cdrs.parquet`
- `~/.opensnow/warehouse/opensnow/public/subscribers.parquet`
- `~/.opensnow/warehouse/opensnow/public/towers.parquet`
- banking sample Parquet files when `--industry both` is used

The server prints:

- Web UI: `http://localhost:8080`
- REST API health: `http://localhost:8080/health`
- REST API status: `http://localhost:8080/api/v1/status`
- PostgreSQL wire: disabled by default. For trusted local compatibility smoke only, start with `cargo run -p opensnow-cli -- start --enable-pgwire` or set `[server].pg_enabled = true`.

## 3. Docker Compose demo

Use this path when the user does not want to install Rust locally.

```bash
docker compose up --build opensnow
```

Then, in another terminal:

```bash
scripts/public-smoke.sh
```

Notes:

- The default compose demo publishes HTTP on `127.0.0.1:8080` only; pgwire remains disabled/unpublished unless you explicitly opt in for trusted-local compatibility testing with `docker compose -f docker-compose.yml -f docker-compose.pgwire.yml up --build opensnow`.
- The current compose file starts the server only; the smoke script creates its own small `public_smoke` table over REST so the demo does not depend on bundled credentials or external data.
- MinIO, Prometheus, and Grafana are present in `docker-compose.yml` for deeper local testing, but the first public test should keep the surface area to `opensnow` unless the user explicitly wants monitoring/storage demos.

## 4. k3d Kubernetes demo

Use k3d to prove Helm, service wiring, health checks, and MinIO-backed object storage locally. Keep pgwire disabled unless you are running a trusted-local compatibility smoke.

```bash
k3d cluster create --config deploy/k3d-config.yaml
helm dependency build deploy/helm/opensnow
helm upgrade --install opensnow deploy/helm/opensnow \
  -f deploy/helm/opensnow/values-dev.yaml \
  --set config.storage.type=s3 \
  --set config.storage.endpoint=http://opensnow-minio:9000 \
  --set worker.replicas=3 \
  --set gateway.type=ClusterIP
kubectl rollout status deploy/opensnow-coordinator --timeout=180s
kubectl port-forward svc/opensnow-gateway 8080:8080
scripts/public-smoke.sh
scripts/quickstart-smoke.sh --mode k3d
```

If the local chart names differ after templating, discover the service first:

```bash
kubectl get svc -l app.kubernetes.io/name=opensnow
```

## 5. First SQL query

The current public SQL contract, sample SQL, request limits, and unsupported SQL
behavior are tracked in `docs/SQL_COMPATIBILITY.md`. In short: the REST query,
CLI local/shell, and pgwire simple-query paths share the same demo
SQL guardrail: one statement per request, trailing semicolon permitted, 64 KiB
SQL-text cap, `COPY INTO` blocked, destructive DDL/DML/transactions blocked,
wrapped destructive materialization queries blocked, and inert comments/strings
with destructive words allowed.

Run the sample-data query without starting the server:

```bash
cargo run -p opensnow-cli -- local "SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type"
```

Run the same table through the interactive shell:

```bash
cargo run -p opensnow-cli -- shell -c "SELECT region, COUNT(*) AS subscribers FROM subscribers GROUP BY region ORDER BY subscribers DESC"
```

Run through the REST API after `cargo run -p opensnow-cli -- start`:

```bash
curl -fsS -H 'content-type: application/json' \
  -d '{"sql":"SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type"}' \
  http://localhost:8080/api/v1/query
```

Run through PostgreSQL wire only in a trusted local session after explicitly enabling it:

```bash
cargo run -p opensnow-cli -- start --enable-pgwire
psql -h localhost -p 5433 -U opensnow -d opensnow \
  -c "SELECT call_type, COUNT(*) AS calls FROM cdrs GROUP BY call_type ORDER BY call_type;"
```

Known limitation: auth-disabled pgwire is a loopback/trusted-local compatibility lane and routes statements with local/demo context only. Auth-enabled pgwire now validates a bearer JWT supplied as the PostgreSQL password, binds startup user/database to JWT subject/tenant, and shares scope/object-policy/audit checks with REST/dbt/MCP, but it still speaks PostgreSQL CleartextPassword and does not terminate TLS. Do not expose it outside local smoke, a port-forward, or an external TLS/source-restricted boundary.

Client compatibility matrix: the pgwire lane is simple-query only today. `psql -c` smoke is supported; Python `psycopg`/`psycopg2` default extended-query execution, COPY protocol, and general BI/dbt adapter pg_catalog introspection are documented unsupported and should return clear errors instead of hanging. Basic `information_schema.tables`, `information_schema.columns`, and REST `/api/v1/dbt/catalog` catalog-shape probes are covered by `scripts/public-smoke.sh` when pgwire is explicitly enabled.

## 6. Pipeline and dashboard tab expectations

The local quickstart server starts without bundled dbt artifacts by default. In that mode `GET /api/v1/pipeline` returns `available:false`, an artifacts path, no nodes, and no configured dashboards; the workspace UI shows a configuration note instead of implying that a missing pipeline can be run immediately. To enable the pipeline/lineage/dashboard demo locally, configure a dbt project and artifacts explicitly:

```bash
export OPENSNOW_DBT_PROJECT_DIR=$PWD/deploy/demo/dbt
export OPENSNOW_DBT_ARTIFACTS_DIR=$PWD/deploy/demo/dbt/target
# optional: export OPENSNOW_DASHBOARD_URL=https://your-bi.example/dashboard/...
```

See `docs/MCP_CONTROL_PLANE.md` for the dbt/MCP control-plane workflow and `deploy/demo/README.md` for the hosted portfolio demo wiring.

## 7. Smoke checks

`scripts/public-smoke.sh` is the public, repeatable smoke pack. It checks:

1. `curl -fsS http://localhost:8080/health`
2. `GET /api/v1/status`
3. `POST /api/v1/ingest` with a three-row `public_smoke` table
4. `POST /api/v1/query` with `SELECT COUNT(*) AS rows FROM public_smoke`
5. Optional trusted-local pgwire smoke via `psql -h localhost -p 5433` simple-query, basic `information_schema.tables`/`information_schema.columns`, REST `/api/v1/dbt/catalog`, clear COPY rejection, and a documented Python PostgreSQL client (`psycopg`/`psycopg2`) extended-query skip/error only when `OPENSNOW_ENABLE_PGWIRE=1`

Run it with defaults:

```bash
scripts/public-smoke.sh
```

Run it against a non-default HTTP port-forward:

```bash
OPENSNOW_BASE_URL=http://localhost:18080 scripts/public-smoke.sh
```

Run the optional trusted-local pgwire smoke after starting with `--enable-pgwire` or `pg_enabled = true`:

```bash
OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_PGPORT=15433 scripts/public-smoke.sh
```

## 8. SSO-ready org auth surface

For public testing, keep auth mode explicit:

- Local public demo: auth is off unless `OPENSNOW_JWT_SECRET` is set. `/health`, `/metrics`, and `/` remain public.
- Token demo: setting `OPENSNOW_JWT_SECRET` enables JWT protection for `/api/v1/query`, `/api/v1/ingest`, and `/api/v1/distributed_query`, plus `POST /auth/token` for OAuth2 client-credentials-style testing.
- Enterprise readiness: do not market the current build as enterprise-auth ready. `docs/ENTERPRISE_AUTH_QA_VALIDATION.md` classifies SSO login/admin APIs, route protection, SQL privilege enforcement, tenant authenticity, SCIM, append-only audit, durable secrets, marketplace entitlements, and Helm/Terraform auth gates as launch blockers.

Public demo guidance:

```bash
# no auth, local only, HTTP/REST only by default
cargo run -p opensnow-cli -- start

# token-protected REST demo; use a development-only secret
OPENSNOW_JWT_SECRET=dev-only-change-me cargo run -p opensnow-cli -- start
```

Do not reuse demo secrets in production or public hosted demos.

## 9. Cloud demo path

Fastest safe hosted path before marketplace submission:

1. Publish a versioned container image to a private or staging registry.
2. Deploy the Helm chart to a small managed Kubernetes cluster.
3. Use managed object storage and managed PostgreSQL catalog:
   - AWS: EKS + S3 + RDS PostgreSQL, with IRSA instead of static S3 keys.
   - GCP: GKE + GCS + Cloud SQL PostgreSQL, with Workload Identity instead of JSON keys.
4. Expose HTTP through a load balancer; keep pgwire disabled for public hosted demos until its auth/tenant/RBAC/audit boundary is implemented.
5. Set `OPENSNOW_JWT_SECRET` or a real OIDC/OAuth front door before sharing a public URL.
6. Run `scripts/public-smoke.sh` from outside the cluster against the public endpoints.
7. Run the QA release checklist in `docs/QA_RELEASE_CHECKLIST.md` before inviting external testers.

Marketplace path is not safe until the enterprise-auth and entitlement blockers are closed. Treat AWS/GCP marketplace docs in `docs/DEPLOYMENT.md` as target packaging, not current public launch approval.

## Remaining launch blockers

P0 before a truly public hosted test:

- Implement real SSO login/admin APIs and make route protection mandatory in hosted mode.
- Enforce SQL privileges/RBAC before query execution, not only at UI/admin layers.
- Replace spoofable tenant headers with signed/org-bound tenant context.
- Add SCIM lifecycle or a documented temporary user-provisioning path.
- Add append-only audit events and export.
- Add durable, rotatable, sealed secrets for service clients and JWT/OIDC configuration.
- Add marketplace identity/entitlement validation for AWS/GCP packaging.
- Add Helm/Terraform values that fail closed when enterprise auth is required but not configured.

P1 usability improvements for external testers:

- Publish a prebuilt image and binary release so `cargo build` is optional.
- Add browser screenshots or a short terminal recording for the first SQL query.
