# External-user OpenSnow demo/quickstart QA validation

Date: 2026-05-28

## Verdict

NOT SIGNED OFF for external-user safe quickstart/demo testability.

The local source quickstart can generate sample data and answer a first SQL query when an explicit existing config is supplied, and a debug binary can serve HTTP health, REST query, and pgwire query locally. However the published docs and container/Kubernetes paths still have blocking external-user issues: unsafe no-auth port publishing, Docker non-root home/config mismatch risk, broken/incorrect Helm quickstart values, malformed SSO admin curl examples, and no one-command smoke/regression wrapper for the advertised quickstart.

## Environment observed

- Workdir: `/path/to/opensnow`
- Docker: `Docker version 29.1.3`
- Docker Compose: `v5.0.0`
- k3d: `v5.8.3`, k3s `v1.31.5-k3s1`
- kubectl client: `v1.35.1`
- helm: not installed in this QA environment
- psql: installed
- Built/debug binary used for smoke: `target/debug/opensnow`, version `0.1.0`

## Commands run and evidence

### 1. Tooling and repo state

```bash
cargo --version
rustc --version
command -v docker
command -v k3d
command -v helm
command -v kubectl
command -v psql
git status --short
```

Result:

- Rust/cargo available after rustup sync.
- Docker, Docker Compose, k3d, kubectl, psql available.
- `helm` absent, so Helm template/install could not be executed locally.
- Repo is actively dirty from other workers; QA did not modify production code.

### 2. Local sample data init

First attempted an isolated external-style run with a non-existent `--config` path:

```bash
HOME=/tmp/opensnow-qa-ext/home OPENSNOW_OTEL_DISABLED=1 \
  cargo run --quiet --bin opensnow -- \
  --config /tmp/opensnow-qa-ext/run/opensnow.toml \
  init --with-sample-data --industry both
```

Actual:

```text
Failed to load config from /tmp/opensnow-qa-ext.6NpUCh/run/opensnow.toml: No such file or directory (os error 2)
```

Then created an explicit temp config and re-ran:

```toml
[server]
http_port = 18080
pg_port = 15433
host = "127.0.0.1"

[storage]
warehouse_path = "/tmp/opensnow-qa-warehouse"

[catalog]
path = "/tmp/opensnow-qa-catalog.db"
```

```bash
OPENSNOW_OTEL_DISABLED=1 cargo run --quiet --bin opensnow -- \
  --config /tmp/opensnow-qa-config.toml \
  init --with-sample-data --industry both
```

Actual: PASS. Generated:

- `/tmp/opensnow-qa-warehouse/opensnow/public/cdrs.parquet` (10,000 rows)
- `/tmp/opensnow-qa-warehouse/opensnow/public/subscribers.parquet` (5,000 rows)
- `/tmp/opensnow-qa-warehouse/opensnow/public/towers.parquet` (500 rows)
- `/tmp/opensnow-qa-warehouse/opensnow/public/transactions.parquet` (50,000 rows)
- `/tmp/opensnow-qa-warehouse/opensnow/public/accounts.parquet` (10,000 rows)
- `/tmp/opensnow-qa-warehouse/opensnow/public/customers.parquet` (5,000 rows)

### 3. First query / local shell

```bash
OPENSNOW_OTEL_DISABLED=1 cargo run --quiet --bin opensnow -- \
  --config /tmp/opensnow-qa-config.toml \
  shell -c "SELECT call_type, COUNT(*) AS n FROM cdrs GROUP BY call_type ORDER BY call_type"
```

Actual: PASS.

```text
+-----------+------+
| call_type | n    |
+-----------+------+
| data      | 3333 |
| sms       | 3333 |
| voice     | 3334 |
+-----------+------+
```

Also verified the existing debug binary:

```bash
target/debug/opensnow --version
OPENSNOW_OTEL_DISABLED=1 target/debug/opensnow --config /tmp/opensnow-qa-config.toml \
  shell -c "SELECT COUNT(*) AS n FROM subscribers"
```

Actual: PASS, `opensnow 0.1.0`, `n = 5000`.

### 4. HTTP health, REST query, pgwire query

Started server on localhost-only temp ports using debug binary:

```bash
OPENSNOW_OTEL_DISABLED=1 target/debug/opensnow \
  --config /tmp/opensnow-qa-config.toml start
```

Smoke commands:

```bash
curl -fsS --max-time 5 http://127.0.0.1:18080/health
curl -fsS --max-time 5 http://127.0.0.1:18080/api/v1/status
curl -fsS --max-time 10 -H 'content-type: application/json' \
  -d '{"sql":"SELECT call_type, COUNT(*) AS n FROM cdrs GROUP BY call_type ORDER BY call_type"}' \
  http://127.0.0.1:18080/api/v1/query
psql -h 127.0.0.1 -p 15433 -U admin -d opensnow \
  -c 'SELECT COUNT(*) AS n FROM cdrs;'
```

Actual: PASS.

```json
{"engine":"opensnow","status":"ok"}
{"engine":"Apache DataFusion","status":"running","version":"0.1.0"}
{"data":"{\"call_type\":\"data\",\"n\":3333}\n{\"call_type\":\"sms\",\"n\":3333}\n{\"call_type\":\"voice\",\"n\":3334}\n","rows":3,"status":"ok"}
```

pgwire result:

```text
   n
-------
 10000
(1 row)
```

Important security observation: REST query succeeded without auth because `OPENSNOW_JWT_SECRET` was not set. This is acceptable only for localhost/dev, but unsafe when docs publish `0.0.0.0`/Docker ports.

### 5. Docker Compose static validation

```bash
docker compose config
```

Actual: PASS for Compose YAML parsing.

Historical rendered facts from the original blocked QA run:

- `opensnow` built from local `Dockerfile` and published `8080:8080`, `5433:5433` on host interfaces. Current deployment docs bind demos to localhost.
- MinIO previously used well-known root credentials; active Helm dev values now use explicit demo-only placeholders.
- Grafana previously used public default admin credentials; verify current dashboard packaging before exposing it outside localhost.

Docker image build/run was not completed in this pass because the source build was already expensive and shared with another active coding worker. Re-run the smoke wrapper against the active quickstart before signoff.

### 6. k3d/Helm static validation

```bash
k3d cluster list
docker compose config
python3 - <<'PY'
from pathlib import Path
vals=Path('deploy/helm/opensnow/values.yaml').read_text()
docs=Path('docs/DEPLOYMENT.md').read_text()
for key in ['workers.replicas','storage.type','auth.adminPassword','minio.enabled','worker.replicas','config.storage.type']:
    print(f'{key}: docs={key in docs} values={key.split('.')[0]+':' in vals or key in vals}')
PY
```

Actual:

```text
workers.replicas: docs=True values=False
storage.type: docs=True values=True
worker.replicas: docs=False values=True
config.storage.type: docs=False values=True
```

Docs command uses `--set workers.replicas=3`, but chart values use `worker.replicas`. Docs command uses `--set storage.type=minio`, but chart config is under `config.storage.type`; top-level `storage` is not consumed by templates. Therefore the published k3d command does not configure the chart as advertised.

`helm` was not installed, so no live `helm template` / `helm install` was executed.

### 7. Auth/SSO tests

```bash
OPENSNOW_OTEL_DISABLED=1 cargo test -p opensnow-auth
OPENSNOW_OTEL_DISABLED=1 cargo test -p opensnow-server auth::tests -- --nocapture
```

Actual:

- `opensnow-auth`: PASS, 30 unit tests + 4 enterprise contract tests.
- `opensnow-server auth::tests`: PASS, 9 tests.

A mistaken command was also run and failed due cargo syntax, not product behavior:

```text
cargo test -p opensnow-server health status auth::tests
error: unexpected argument 'status' found
```

## Blocking findings

### B1. Historical: published Docker quickstart exposed unauthenticated query surfaces on all host interfaces

Severity: High
Area: security / external-user safety
Evidence:

- Original `docs/DEPLOYMENT.md` recommended a Docker command that bound `8080`/`5433` on all host interfaces and mounted `/root/.opensnow`. Active docs have since moved external testers to `scripts/demo.sh` / `scripts/quickstart-smoke.sh` and localhost-bound Docker examples.

- Server binds `0.0.0.0` by implementation in `crates/opensnow-server/src/server.rs`.
- Auth is optional and disabled unless `OPENSNOW_JWT_SECRET` is set.
- Local smoke confirmed unauthenticated `/api/v1/query` works.

Expected:

External demo docs should either bind to localhost (`127.0.0.1:8080:8080`, `127.0.0.1:5433:5433`) or enable a generated demo secret/client by default for published Docker/Kubernetes demos.

Current status:

This was a valid blocker for the original QA run. Re-test active docs with `scripts/quickstart-smoke.sh --mode docker` and confirm hosted demos still set JWT/secret-manager credentials before external exposure.

### B2. Dockerfile runs as UID 1000 but docs mount `/root/.opensnow`; default paths point to `/root/.opensnow`

Severity: High
Area: Docker quickstart reliability
Evidence:

- `Dockerfile`:

```dockerfile
USER 1000
ENTRYPOINT ["opensnow"]
CMD ["start"]
```

- docs Docker command mounts `~/.opensnow:/root/.opensnow`.
- default config/docs use `~/.opensnow/warehouse` and `~/.opensnow/catalog.db`.

Expected:

Container should have a writable non-root home (for example `/home/opensnow/.opensnow`) and docs should mount that path, or the image should set `HOME` and data dir explicitly.

Actual:

The advertised mount path is root-owned/root-home oriented while the process runs as UID 1000. This is likely to fail or silently use an unexpected home/path depending on image env/passwd behavior.

### B3. Historical: k3d/Helm quickstart values were wrong and likely no-op

Severity: High
Area: Kubernetes demo testability
Evidence:

Original docs used ignored Helm keys (`storage.type`, `workers.replicas`). Active deployment docs now use `config.storage.type`, `config.storage.endpoint`, and `worker.replicas`; keep this QA note as historical evidence, not a current blocker without re-test.

Chart values/templates use:

- `worker.replicas` (singular), not `workers.replicas`
- `config.storage.type`, not top-level `storage.type`

Expected:

Published command should set keys consumed by chart templates and should include dev values if needed:

```bash
helm upgrade --install opensnow deploy/helm/opensnow \
  -f deploy/helm/opensnow/values-dev.yaml \
  --set config.storage.type=s3 \
  --set config.storage.endpoint=http://opensnow-minio:9000 \
  --set worker.replicas=3
```

Current status:

The documented override names have been updated. Re-run the k3d/Helm smoke before signoff.

### B4. k3d docs conflict with k3d config cluster name

Severity: Medium
Area: Kubernetes demo ergonomics
Evidence:

- `deploy/k3d-config.yaml` metadata name is `opensnow-dev`.
- Docs command uses `k3d cluster create opensnow --config deploy/k3d-config.yaml`.

Expected:

Docs should use either `k3d cluster create --config deploy/k3d-config.yaml` or align the config name with the command.

Actual:

External users may end up with confusing cluster naming or command behavior depending on k3d argument precedence.

### B5. SSO/admin API docs contain malformed curl headers and unverified endpoints

Severity: Medium
Area: docs accuracy / enterprise auth expectations
Evidence:

`docs/DEPLOYMENT.md` SSO examples include missing closing quotes in `Authorization` headers:

```bash
-H "Authorization: Bearer *** \
```

The admin API paths in the docs need endpoint-level verification against server routes before being presented as runnable quickstart commands.

Expected:

Examples should be copy/paste valid, use `Authorization: Bearer $TOKEN`, and be covered by a smoke test.

Actual:

The examples are malformed and not safe as external-user instructions.

### B6. No checked-in one-command external quickstart smoke/regression

Severity: Medium
Area: regression coverage
Evidence:

Manual QA needed custom temp config and ad hoc commands. Existing scripts include `scripts/e2e-k8s-smoke.sh`, but the advertised local/Docker/k3d quickstart does not have a single CI-backed smoke that runs:

- init sample data
- first shell query
- server health
- REST query
- pgwire query
- Docker run/compose health
- k3d/Helm install + port-forward smoke

Expected:

A script like `scripts/quickstart-smoke.sh --mode local|docker|k3d` should back docs and CI.

Actual:

Docs and implementation can drift; this QA pass found drift.

## Non-blocking findings / warnings

### W1. Build/test output has unused warnings

Severity: Low
Area: code hygiene
Evidence:

- `opensnow-core/src/engine_handle.rs`: unused `Arc`, `Mutex`
- `opensnow-core/src/engine.rs`: unused cloud tuning constants
- `opensnow-distributed/src/scheduler.rs`: unused `now`
- `opensnow-distributed/src/operator.rs`: unnecessary `mut`
- `opensnow-server/src/pg.rs`: unused `OpenSnowEngine`

Expected:

Clean quickstart/test output or warnings documented as known.

### W2. `--config <missing>` fails before `init` can create a config

Severity: Low/Medium
Area: first-run UX
Evidence:

`opensnow --config /tmp/new/opensnow.toml init --with-sample-data` fails if the config file does not exist.

Expected:

Either docs avoid `--config` for first init, or `init` creates the target config path when supplied.

Actual:

Supplying a missing config path aborts before init.

## What passed

- Explicit-config source quickstart sample data generation.
- First SQL query through `opensnow shell -c`.
- HTTP `/health` and `/api/v1/status`.
- Unauthenticated REST query in local dev mode.
- pgwire query via `psql`.
- Docker Compose YAML parses.
- `opensnow-auth` tests and server auth unit tests pass.

## Signoff gate

This document records an earlier blocked QA pass. Do not treat B1/B3/default-secret observations as current without re-running the minimum re-test below against the active docs and chart.

Minimum re-test after remediation:

```bash
scripts/quickstart-smoke.sh --mode local
scripts/quickstart-smoke.sh --mode docker
scripts/quickstart-smoke.sh --mode k3d
cargo test -p opensnow-auth
cargo test -p opensnow-server auth::tests -- --nocapture
```
