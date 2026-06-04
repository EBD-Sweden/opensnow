# OpenSnow QA / Production & Cloud Readiness Checklist

Scope: release signoff checklist for OpenSnow local binary, Docker, k3d/Helm, AWS, GCP, SQL wire compatibility, benchmark smoke, and marketplace readiness. This document is QA/evaluation only; production implementation fixes belong to the CTO/engineering lane.

Source docs reviewed:
- `docs/DEPLOYMENT.md`
- `deploy/helm/opensnow/*`
- `deploy/terraform/*`
- `bench/README.md`
- `bench/BENCHMARK_PLAN.md`
- `db/README.md`
- `ARCHITECTURE.md`
- `Dockerfile`
- `tests/e2e/*`

## Signoff rule

A release candidate is production/cloud ready only when every P0/P1 gate below is PASS or has an explicitly accepted waiver. P2 items may ship if documented in release notes and not marketplace-blocking.

Severity classification:
- P0 blocker: install/start/query, data integrity, auth bypass, destructive infra default, or marketplace one-click path cannot work.
- P1 blocker: documented production path is broken, Helm/Terraform validation fails, SQL client compatibility fails, benchmark smoke cannot run, or security default is unsafe for public exposure.
- P2 issue: docs mismatch, non-critical observability/cost polish, optional benchmark/provider path missing.
- P3 follow-up: usability improvement or non-release-critical cleanup.

Standard evidence format for every failed gate:

```text
Gate: <section + id>
Severity: P0/P1/P2/P3
Environment: <local/docker/k3d/aws/gcp>
Command(s): <exact commands>
Expected: <expected result>
Actual: <actual output or error excerpt>
Artifact: <log path, screenshot path, CSV, kubectl describe, terraform plan path>
Owner: <implementation owner> / <QA retest owner>
```

## 0. Preflight / repository hygiene

Run before environment-specific gates:

```bash
git status --short
cargo --version
docker --version
kubectl version --client
k3d version
psql --version
# Optional/cloud gates:
helm version
terraform version
aws --version
gcloud version
duckdb --version
```

Pass criteria:
- Worktree contains only intentional release artifacts.
- Required tools exist for the path being certified.
- No secrets are committed or printed in logs.

Blockers:
- P0: dirty release artifacts include secrets, credentials, or generated warehouse data.
- P1: required release path lacks a documented tool prerequisite.
- P2: optional benchmark/cloud tools absent on the QA host; mark that path not executed.

## 1. Build, unit, and e2e baseline

Commands:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p opensnow-e2e-tests --test integration
cargo test -p opensnow-e2e-tests --test auth
cargo build --release
./target/release/opensnow --help
./target/release/opensnow-mcp --help
```

Pass criteria:
- Format, clippy, workspace tests, and e2e tests pass.
- Release binaries exist at `target/release/opensnow` and `target/release/opensnow-mcp`.
- Help output documents `init`, `start`, `local`, `shell`, `status`, `queries`, `operator-plan`, and `operator-apply`.

Blockers:
- P0: release binary does not build or crashes on `--help`.
- P0: e2e health/query/auth tests fail.
- P1: clippy/test failures in non-release-critical crates.
- P2: docs mention commands that do not exist in CLI help.

Known doc mismatch to check during this gate:
- `docs/DEPLOYMENT.md` and `ARCHITECTURE.md` mention `opensnow bench tpch`; current CLI source does not define a `bench` subcommand.
- `docs/DEPLOYMENT.md` uses `opensnow shell --exec`; current CLI supports `opensnow shell -c/--command`.

## 2. Local single-binary gate

Use an isolated home/config so QA does not mutate developer state:

```bash
TMP_HOME=$(mktemp -d)
TMP_CFG=$(mktemp -d)/opensnow.toml
cat > "$TMP_CFG" <<EOF
[server]
http_port = 18080
pg_port = 15433
host = "127.0.0.1"

[storage]
warehouse_path = "$TMP_HOME/warehouse"

[catalog]
path = "$TMP_HOME/catalog.db"
EOF

./target/release/opensnow --config "$TMP_CFG" init --with-sample-data --industry both
OPENSNOW_OTEL_DISABLED=1 ./target/release/opensnow --config "$TMP_CFG" start --http-port 18080 --pg-port 15433 > /tmp/opensnow-local.log 2>&1 &
OS_PID=$!
trap 'kill $OS_PID || true' EXIT

for i in $(seq 1 30); do
  curl -sf http://127.0.0.1:18080/health && break
  sleep 1
done

curl -sf http://127.0.0.1:18080/health
curl -sf http://127.0.0.1:18080/metrics | grep -E 'opensnow_(queries_total|query_duration_seconds|active_warehouses)'

curl -sf -X POST http://127.0.0.1:18080/api/v1/query \
  -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT COUNT(*) AS n FROM cdrs"}' | tee /tmp/opensnow-local-query.json

grep -q '"status":"ok"' /tmp/opensnow-local-query.json
grep -q '"n":10000' /tmp/opensnow-local-query.json

./target/release/opensnow --config "$TMP_CFG" shell -c "SELECT COUNT(*) AS n FROM subscribers"
./target/release/opensnow --config "$TMP_CFG" local "SELECT 1 AS smoke"
./target/release/opensnow status --host 127.0.0.1 --port 18080
```

Pass criteria:
- `init --with-sample-data --industry both` creates telecom and banking Parquet files.
- `/health` returns status ok.
- `/metrics` exposes the documented Prometheus metric names.
- REST query, shell query, and local query all succeed.
- Catalog file and warehouse files are created under the isolated temp paths.

Blockers:
- P0: server cannot start or health never becomes ok.
- P0: sample data count queries return wrong counts.
- P1: metrics endpoint missing documented metrics.
- P1: CLI shell/local path differs from docs.

## 3. PostgreSQL wire compatibility gate

The docs promise PostgreSQL wire compatibility on port 5433. Validate using stock `psql` and the default PG client behavior.

Commands after the local server is running:

```bash
export PGPASSWORD="${OPENSNOW_QA_ADMIN_PASSWORD:-OPEN/SNOW/DEMO/ONLY}"
psql 'host=127.0.0.1 port=15433 user=admin dbname=opensnow sslmode=disable' -c 'SELECT 1 AS smoke;'
psql 'host=127.0.0.1 port=15433 user=admin dbname=opensnow sslmode=disable' -c 'SELECT COUNT(*) FROM cdrs;'
psql 'host=127.0.0.1 port=15433 user=admin dbname=opensnow sslmode=disable' -c '\dt'
psql 'host=127.0.0.1 port=15433 user=admin dbname=opensnow sslmode=disable' -c 'SELECT table_name FROM information_schema.tables LIMIT 10;'
```

Pass criteria:
- Authentication outcome matches documented local auth mode.
- `psql` can connect, execute a scalar query, query sample tables, and introspect tables.
- Errors are PostgreSQL-client-readable and do not drop the process.

Blockers:
- P0: `psql` cannot complete startup/auth handshake.
- P0: valid SELECT crashes the server.
- P1: `information_schema` / `pg_catalog` introspection is absent enough to break BI/dbt clients.
- P2: minor psql formatting or notices differ from PostgreSQL.

## 4. Docker image gate

Commands:

```bash
docker build -t opensnow:qa .
DOCKER_HOME=$(mktemp -d)
docker run --rm -d --name opensnow-qa \
  -p 18081:8080 -p 15434:5433 \
  -v "$DOCKER_HOME:/home/opensnow/.opensnow" \
  opensnow:qa

for i in $(seq 1 30); do
  curl -sf http://127.0.0.1:18081/health && break
  sleep 1
done
curl -sf http://127.0.0.1:18081/health
curl -sf http://127.0.0.1:18081/metrics | grep opensnow_queries_total
PGPASSWORD="${OPENSNOW_QA_ADMIN_PASSWORD:-OPEN/SNOW/DEMO/ONLY}" psql 'host=127.0.0.1 port=15434 user=admin dbname=opensnow sslmode=disable' -c 'SELECT 1;'
docker logs opensnow-qa --tail 100
docker stop opensnow-qa
```

Pass criteria:
- Image builds from the repository Dockerfile.
- Container starts as non-root user and exposes HTTP 8080, PG 5433, MCP 8090.
- Health, metrics, and PG smoke pass.
- Volume mount is usable by user `1000` or docs specify required host permissions.

Blockers:
- P0: image build fails or binary missing from final image.
- P0: container cannot write catalog/warehouse due to non-root volume permissions.
- P1: documented Docker command mounts `/root/.opensnow` while image runs as `USER 1000`; verify path/permissions or fix docs.
- P1: published tag `opensnow/opensnow:<release>` missing for Helm/default docs.

## 5. Helm chart static gate

Commands:

```bash
# If helm is installed:
helm dependency build deploy/helm/opensnow
helm lint deploy/helm/opensnow
helm template opensnow deploy/helm/opensnow --namespace opensnow > /tmp/opensnow-rendered.yaml
helm template opensnow deploy/helm/opensnow --namespace opensnow -f deploy/helm/opensnow/values-dev.yaml > /tmp/opensnow-rendered-dev.yaml
kubectl apply --dry-run=client -f /tmp/opensnow-rendered.yaml
kubectl apply --dry-run=client -f /tmp/opensnow-rendered-dev.yaml

# Secret/default review (expected to return no active production defaults; release-checklist patterns are negative tests):
grep -RInE "admin_password\s*=\s*\"admin\"|password:\s*(admin|opensnow|minioadmin)\b|rootPassword:\s*minioadmin|MINIO_ROOT_(USER|PASSWORD)=minioadmin|tag:\s*latest" \
  opensnow.toml deploy/helm/opensnow Dockerfile crates db
```

Pass criteria:
- Dependencies resolve or vendored chart archives are intentionally pinned.
- `helm lint` and client-side dry-run pass for default and dev values.
- Rendered resources include coordinator Deployment, worker StatefulSet, gateway Service, metadata Postgres if builtin enabled, NetworkPolicies, optional MCP, optional KEDA.
- No production defaults use public/default passwords, `latest`, or unauthenticated storage without a release note/override gate.

Blockers:
- P0: Helm template/lint fails for default values.
- P0: production Helm values or rendered manifests expose known password defaults (`admin`, `opensnow`, `minioadmin`) instead of generated/existing-secret paths.
- P1: docs use value names not consumed by chart. Verify `worker.replicas`, `config.storage.*`, and Terraform README examples against chart values before release.
- P1: chart dependency condition for PostgreSQL must match intended builtin/external behavior.

## 6. k3d / Helm runtime gate

Commands:

```bash
k3d cluster create opensnow-dev --config deploy/k3d-config.yaml
kubectl create namespace opensnow --dry-run=client -o yaml | kubectl apply -f -

# Use local QA image when testing an unpublished release.
docker build -t localhost:5111/opensnow:qa .
docker push localhost:5111/opensnow:qa

helm upgrade --install opensnow deploy/helm/opensnow \
  --namespace opensnow \
  -f deploy/helm/opensnow/values-dev.yaml \
  --set image.repository=localhost:5111/opensnow \
  --set image.tag=qa \
  --set image.pullPolicy=Always \
  --set gateway.type=NodePort \
  --set minio.enabled=true \
  --set config.storage.type=s3 \
  --set config.storage.endpoint=http://opensnow-minio:9000 \
  --set config.storage.bucket=opensnow-data

kubectl -n opensnow rollout status deploy/opensnow-coordinator --timeout=180s
kubectl -n opensnow rollout status statefulset/opensnow-worker --timeout=180s
kubectl -n opensnow get pods,svc,pvc
curl -sf http://127.0.0.1:8080/health
curl -sf -X POST http://127.0.0.1:8080/api/v1/query -H 'Content-Type: application/json' -d '{"sql":"SELECT 1 AS smoke"}'
PGPASSWORD="${OPENSNOW_QA_ADMIN_PASSWORD:-OPEN/SNOW/DEMO/ONLY}" psql 'host=127.0.0.1 port=5433 user=admin dbname=opensnow sslmode=disable' -c 'SELECT 1;'
kubectl -n opensnow logs deploy/opensnow-coordinator --tail=200

helm uninstall opensnow -n opensnow
k3d cluster delete opensnow-dev
```

Pass criteria:
- k3d config creates cluster and local registry.
- Chart installs with dev values and local image.
- Pods become Ready, PVCs bind, gateway ports map to localhost 8080/5433.
- REST and PG smoke pass from host.
- Uninstall leaves no stuck release resources except intentional local volume/cache.

Blockers:
- P0: chart cannot install or core pods crashloop.
- P0: REST/PG smoke fails inside k3d.
- P1: MinIO/Postgres dependencies cannot initialize with chart defaults.
- P1: NetworkPolicy blocks required coordinator-worker-metadata traffic.
- P2: uninstall cleanup leaves non-critical resources.

## 7. AWS Terraform + EKS/Helm gate

Static validation commands, no cloud spend:

```bash
cd deploy/terraform
terraform fmt -check -recursive
terraform init -backend=false
terraform validate
terraform plan -out=/tmp/opensnow-aws.tfplan \
  -var="warehouse_bucket=opensnow-qa-$(date +%s)" \
  -var="cluster_name=opensnow-qa"
terraform show -json /tmp/opensnow-aws.tfplan > /tmp/opensnow-aws.tfplan.json
```

Cloud runtime commands, only with approved AWS account/budget:

```bash
cd deploy/terraform
terraform apply -var="warehouse_bucket=<globally-unique-bucket>" -var="cluster_name=opensnow-qa"
$(terraform output -raw kubeconfig_command)
helm upgrade --install opensnow ../helm/opensnow \
  --namespace opensnow --create-namespace \
  --set serviceAccount.annotations."eks\.amazonaws\.com/role-arn"="$(terraform output -raw irsa_role_arn)" \
  --set config.storage.type=s3 \
  --set config.storage.bucket="$(terraform output -raw warehouse_bucket)" \
  --set config.storage.region="$(terraform output -raw region)"
kubectl -n opensnow rollout status deploy/opensnow-coordinator --timeout=300s
curl -sf "$(kubectl -n opensnow get svc opensnow-gateway -o jsonpath='{.status.loadBalancer.ingress[0].hostname}')/health"
terraform destroy
```

Pass criteria:
- Terraform fmt/init/validate/plan pass.
- Plan includes EKS, private subnets, encrypted/versioned S3, public access block, IRSA role scoped to OpenSnow service account, and least-privilege S3 access.
- Runtime install reaches Ready and can read/write warehouse bucket through IRSA without static access keys.
- Destroy succeeds or retains warehouse bucket only when non-empty by design.

Blockers:
- P0: Terraform validation/plan fails.
- P0: IAM permits broad `s3:*` or public bucket access.
- P1: docs claim RDS, ALB/NLB, Secrets Manager, CloudWatch, or S3 Gateway Endpoint are provisioned but current Terraform does not provision them. Either implement or downgrade docs before marketplace claims.
- P1: Terraform README value names do not match chart values (`storage.warehouseBucket` vs `config.storage.bucket`).
- P2: no remote state/backend guidance for production.

## 8. GCP Terraform + GKE/Helm gate

Static validation commands:

```bash
cd deploy/terraform/gcp
terraform fmt -check
terraform init -backend=false
terraform validate
terraform plan -out=/tmp/opensnow-gcp.tfplan \
  -var="project_id=<qa-project>" \
  -var="warehouse_bucket=opensnow-qa-$(date +%s)" \
  -var="cluster_name=opensnow-qa"
terraform show -json /tmp/opensnow-gcp.tfplan > /tmp/opensnow-gcp.tfplan.json
```

Cloud runtime commands, only with approved GCP project/budget:

```bash
cd deploy/terraform/gcp
terraform apply \
  -var="project_id=<qa-project>" \
  -var="warehouse_bucket=<globally-unique-bucket>" \
  -var="cluster_name=opensnow-qa"
gcloud container clusters get-credentials "$(terraform output -raw cluster_name)" \
  --region "$(terraform output -raw region)" \
  --project "$(terraform output -raw project_id)"
helm upgrade --install opensnow ../../helm/opensnow \
  --namespace opensnow --create-namespace \
  --set serviceAccount.annotations."iam\.gke\.io/gcp-service-account"="$(terraform output -raw workload_identity_email)" \
  --set config.storage.type=gcs \
  --set config.storage.bucket="$(terraform output -raw warehouse_bucket)"
kubectl -n opensnow rollout status deploy/opensnow-coordinator --timeout=300s
kubectl -n opensnow get pods,svc
terraform destroy
```

Pass criteria:
- Terraform fmt/init/validate/plan pass.
- Plan includes GKE Autopilot, versioned GCS bucket with uniform access, Google service account, Workload Identity binding scoped to namespace/service account, and objectAdmin only on warehouse bucket.
- Runtime install reaches Ready and can read/write GCS through Workload Identity.

Blockers:
- P0: Terraform validation/plan fails.
- P0: Workload Identity binding wrong or pods require key files.
- P1: docs claim Cloud SQL, Cloud Load Balancing, Secret Manager, or explicit Private Google Access resources are provisioned but current Terraform only provisions GKE/GCS/Workload Identity. Either implement or downgrade docs before marketplace claims.
- P1: chart values in docs do not match rendered templates.
- P2: no production state/backend guidance.

## 9. PostgreSQL catalog / migration compatibility gate

Commands:

```bash
createdb opensnow_qa || true
export DATABASE_URL=postgresql://$(whoami)@localhost:5432/opensnow_qa
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f db/migrations/000_core.sql
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -f crates/opensnow-auth/migrations/001_sso.sql
psql "$DATABASE_URL" -c '\dt'
psql "$DATABASE_URL" -c "SELECT name FROM roles ORDER BY name;"
psql "$DATABASE_URL" -c "INSERT INTO tenants (id, slug, name) VALUES (gen_random_uuid(), 'qa', 'QA Tenant') RETURNING id;"
```

Pass criteria:
- Migrations run idempotently on a clean PostgreSQL database.
- Core auth/roles/tenants/warehouses and SSO tables exist with required indexes/constraints.
- Docs reference the correct migration paths (`db/migrations/001_sso.sql` currently does not exist; SSO migration is under `crates/opensnow-auth/migrations/001_sso.sql`).

Blockers:
- P0: migration fails on supported PostgreSQL.
- P0: required tables missing or schema differs from code expectations.
- P1: docs point operators at missing migration files.
- P2: seed/default roles differ between SQLite and PostgreSQL.

## 10. SQL compatibility smoke gate

Run through REST and PG wire where supported:

```bash
# REST helper
osql() { curl -sf -X POST http://127.0.0.1:18080/api/v1/query -H 'Content-Type: application/json' -d "$(python3 -c 'import json,sys; print(json.dumps({"sql": sys.stdin.read()}))')"; }

printf 'SELECT 1 AS smoke' | osql
printf 'SELECT COUNT(*) FROM cdrs' | osql
printf 'SELECT call_type, COUNT(*) FROM cdrs GROUP BY call_type ORDER BY call_type' | osql
printf 'SELECT s.plan, COUNT(*) FROM subscribers s JOIN cdrs c ON s.phone = c.caller GROUP BY s.plan ORDER BY s.plan' | osql
printf 'SELECT * FROM cdrs ORDER BY duration_seconds DESC LIMIT 5' | osql
printf 'SELECT DATE_TRUNC('\''hour'\'', timestamp) AS hour, COUNT(*) FROM cdrs GROUP BY hour ORDER BY hour LIMIT 5' | osql
printf 'SELECT invalid FROM missing_table' | osql
```

Pass criteria:
- Basic SELECT, aggregation, join, order/limit, date functions, and invalid-query error path behave deterministically.
- Query logs/metrics increment for success and failure.
- REST error bodies include useful message and do not return stack traces/secrets.

Blockers:
- P0: valid smoke SQL fails or corrupts server state.
- P0: invalid SQL crashes server.
- P1: documented Snowflake-compatible syntax (`COPY INTO`, `CREATE EXTERNAL TABLE`, `QUALIFY`, `TRY_CAST`, etc.) is advertised as ready but fails in smoke; classify each missing feature as implemented/not implemented in release notes.
- P1: REST and PG wire disagree on query result semantics.

## 11. Benchmark smoke gate

Do not run full 14.8GB/3B-row benchmarks for every release candidate. Run fast smoke on every RC, full benchmark only on publishable benchmark runs.

Prerequisites:

```bash
duckdb --version
curl -sf http://127.0.0.1:18080/health
```

Fast smoke commands:

```bash
# TPC-H tiny smoke: exercises data generation, OpenSnow REST query path, DuckDB comparison.
python3 bench/run_tpch_benchmark.sh --sf 0.01 --runs 1 --queries 1,6

# ClickBench script smoke: use small sample only; do not download 14.8GB in routine RC gate.
DATASET_SIZE=small RUNS=2 bash bench/clickbench.sh

# NYC Taxi smoke: OpenSnow only unless AWS/Athena credentials and output bucket are explicitly configured.
bash bench/nyc_taxi.sh --year 2023
```

Full benchmark commands for publishable numbers:

```bash
python3 bench/run_tpch_benchmark.sh --sf 1 --runs 3
python3 bench/run_tpch_benchmark.sh --sf 10 --runs 3
RUNS=5 DATASET_SIZE=full bash bench/clickbench.sh
AWS_REGION=us-east-1 ATHENA_OUTPUT=s3://<qa-athena-results>/ bash bench/nyc_taxi.sh --athena --years 2019,2020,2021,2022,2023
```

Pass criteria:
- Smoke runs finish and write CSVs to `/tmp/opensnow-bench-results/`.
- Failed queries are recorded and classified rather than hidden by timing output.
- Full benchmark records environment, hardware, OpenSnow commit/tag, dataset size, run count, cold/warm behavior, and cost model.

Blockers:
- P0: benchmark smoke cannot query OpenSnow at all.
- P1: scripts have arithmetic/argument parsing failures or false-positive success on failed OpenSnow curl calls.
- P1: Athena comparison is advertised without a valid Glue/external table setup and output bucket instructions.
- P2: `bench/README.md` says `bash bench/nyc_taxi.sh --engine ...`, but script supports `--athena`, `--year`, `--years`, and `--athena-output` only.

## 12. Observability and operations gate

Commands:

```bash
curl -sf http://127.0.0.1:18080/metrics | tee /tmp/opensnow-metrics.txt
grep opensnow_queries_total /tmp/opensnow-metrics.txt
grep opensnow_query_duration_seconds /tmp/opensnow-metrics.txt
grep opensnow_active_warehouses /tmp/opensnow-metrics.txt
kubectl apply --dry-run=client -f deploy/grafana/
kubectl apply --dry-run=client -f deploy/prometheus/prometheus.yml || true
```

Pass criteria:
- Health endpoint, status command, metrics endpoint, Grafana dashboard, and Prometheus config are internally consistent.
- Metrics include success/error labels sufficient for alerting.
- Logs include startup config summary without secrets.

Blockers:
- P0: no health endpoint for orchestrator probes.
- P1: metrics documented in deployment guide are not emitted.
- P1: probes in Helm chart hit paths/ports not served by binary.
- P2: dashboard references missing metric names.

## 13. Security and production-default gate

Checks:

```bash
grep -RInE "admin_password\s*=\s*\"admin\"|password:\s*(admin|opensnow|minioadmin)\b|rootPassword:\s*minioadmin|MINIO_ROOT_(USER|PASSWORD)=minioadmin|tag:\s*latest" \
  opensnow.toml deploy/helm/opensnow Dockerfile crates db | tee /tmp/opensnow-default-secret-grep.txt
helm template opensnow deploy/helm/opensnow --namespace opensnow > /tmp/opensnow-security-rendered.yaml
! grep -nE "password: (admin|opensnow|minioadmin)\b|rootPassword: minioadmin|tag: latest|MINIO_ROOT_(USER|PASSWORD)=minioadmin" /tmp/opensnow-security-rendered.yaml
```

Pass criteria:
- Production path forces or clearly documents setting admin password/JWT secret/catalog password/storage credentials.
- TLS termination story is explicit for public cloud/marketplace.
- NetworkPolicy restricts internal traffic without blocking health/probes.
- Images are pinned by release tag/digest for marketplace paths.
- No long-lived cloud storage credentials are needed when IRSA/Workload Identity is configured.

Blockers:
- P0: default public deployment has known admin password or no auth.
- P0: storage bucket/container is public or broad IAM is generated.
- P1: TLS/auth are optional in marketplace default without warning/forced override.
- P1: secrets appear in ConfigMaps, logs, or rendered manifests instead of Secrets/external secret managers.

## 14. Marketplace readiness gates

AWS Marketplace minimum checklist:
- [ ] Published container image is release-tagged and vulnerability-scanned.
- [ ] CloudFormation/Terraform/Helm deployment path is a single guided flow, not a stale `opensnow-cloud/aws/terraform` path if the repo uses `deploy/terraform`.
- [ ] EKS, S3, RDS/Postgres/catalog, load balancers, IAM/IRSA, secrets, and monitoring claims match actual IaC.
- [ ] `terraform destroy`/uninstall guidance protects customer data bucket by default.
- [ ] Default deployment requires unique admin password/JWT secret and TLS or private endpoint guidance.
- [ ] Private S3 endpoint/VPC endpoint claim is either implemented or removed from marketplace copy.

GCP Marketplace minimum checklist:
- [ ] Published image and Helm chart are release-tagged.
- [ ] GKE/GCS/Cloud SQL/Load Balancing/Secret Manager claims match actual IaC.
- [ ] Workload Identity is configured without key files.
- [ ] Bucket uniform access/versioning and data-retention behavior are documented.
- [ ] Default deployment requires unique admin password/JWT secret and TLS/Ingress guidance.

Blockers:
- P0: one-click marketplace install cannot reach a healthy `/health` endpoint.
- P0: marketplace default exposes admin access, unauthenticated SQL, or public object storage.
- P1: marketplace listing claims managed services not provisioned by actual templates.
- P1: no support/upgrade/uninstall/customer-data-preservation path.

## 15. Release signoff matrix

Fill for each candidate:

| Gate | Status | Severity if fail | Evidence |
|---|---|---:|---|
| Build/unit/e2e | TODO | P0 | |
| Local binary | TODO | P0 | |
| PG wire | TODO | P0 | |
| Docker | TODO | P0 | |
| Helm static | TODO | P0/P1 | |
| k3d runtime | TODO | P0 | |
| AWS Terraform static | TODO | P0/P1 | |
| AWS runtime | TODO/WAIVED | P0/P1 | |
| GCP Terraform static | TODO | P0/P1 | |
| GCP runtime | TODO/WAIVED | P0/P1 | |
| Postgres migrations | TODO | P0/P1 | |
| SQL compatibility smoke | TODO | P0/P1 | |
| Benchmark smoke | TODO | P1 | |
| Observability | TODO | P1 | |
| Security defaults | TODO | P0/P1 | |
| AWS Marketplace | TODO/WAIVED | P0/P1 | |
| GCP Marketplace | TODO/WAIVED | P0/P1 | |

## 16. Initial blocker candidates found during checklist creation

These are not final failures until reproduced with the commands above, but they should be triaged before release signoff:

1. P1 docs/IaC path drift: `docs/DEPLOYMENT.md` now points AWS/GCP to `deploy/terraform` and `deploy/terraform/gcp`; Azure remains explicitly TODO until Azure Terraform packaging is checked in.
2. P1 Helm values drift: active deployment guide examples now use `worker.*` and `config.storage.*`; continue verifying Terraform README/chart examples against chart values before release.
3. P1 cloud claims drift: deployment guide says AWS provisions RDS/ALB/NLB/Secrets Manager/CloudWatch/S3 Gateway Endpoint; current `deploy/terraform/main.tf` visibly provisions EKS, VPC/NAT, S3, and IRSA only. GCP guide similarly claims Cloud SQL/Load Balancing/Secret Manager beyond current `gcp/main.tf`.
4. P1 migration path drift: deployment guide says `db/migrations/001_sso.sql`, but SSO migration exists at `crates/opensnow-auth/migrations/001_sso.sql`; `db/README.md` has both variants in different sections.
5. P1 CLI docs drift: docs mention `opensnow bench tpch` and `opensnow shell --exec`; current CLI source supports benchmark scripts under `bench/` and `opensnow shell -c/--command`.
6. P1 Docker volume/doc drift: Dockerfile runs final image as `USER 1000`, while deployment doc example mounts `~/.opensnow` to `/root/.opensnow`; validate write permissions and expected config path.
7. P1 benchmark docs drift: `bench/README.md` documents `bench/nyc_taxi.sh --engine opensnow|athena|--compare`, but script supports `--athena`, `--year`, `--years`, and `--athena-output`.
8. P0/P1 production default risk: active docs/configs no longer present `admin`/`opensnow`/`minioadmin` as copy/paste defaults, but marketplace/prod paths still must force generated secrets or existing secret-manager/KMS references and reject demo placeholders.
