# Self-Host Quickstart: Query Your Own S3 Data

This guide walks the shortest path from a fresh clone to querying your own data:

> **clone the repo → point OpenSnow at YOUR AWS S3 (or S3-compatible) bucket → SELECT your Parquet.**

It documents the *read/register* path for external object storage and, when
`warehouse_path` is an object-store URL, the materialization write path for
`CREATE TABLE AS SELECT`, materialized views, and trusted `COPY INTO`.
Every flag, env var, and config key below is cited to the source that defines it.

> **Status / scope.** OpenSnow is pre-1.0 (see `README.md`). This guide covers
> querying *existing* `s3://` Parquet and optionally using the configured bucket
> as the warehouse root for materialized outputs — see [Current limitations](#current-limitations).

---

## 1. Prerequisites

- A recent stable Rust toolchain. The repo pins one via `rust-toolchain.toml`,
  so `rustup` installs the correct version automatically (edition 2024).
  (`README.md` › Prerequisites.)
- `python3` (used by the demo/smoke scripts; optional for this path).
- Optional: Docker / `docker compose` to run the container image instead of a
  native binary.
- An AWS S3 (or S3-compatible, e.g. MinIO / Cloudflare R2) bucket containing
  Parquet files you can read, plus credentials *or* an attached IAM role.

---

## 2. Build from source (or Docker image)

### Build from source (recommended today)

```bash
git clone https://github.com/opensnow/opensnow
cd opensnow
cargo build --release
cp target/release/opensnow /usr/local/bin/   # optional: put it on PATH
```

(Commands from `docs/DEPLOYMENT.md` › Install › Option A. `rustup` picks up the
pinned toolchain from `rust-toolchain.toml` on first build.)

### Or build / run the Docker image

```bash
docker build -t opensnow:local .
docker run \
  -p 127.0.0.1:8080:8080 \
  -v ~/.opensnow:/home/opensnow/.opensnow \
  opensnow:local
```

(From `docs/DEPLOYMENT.md` › Install › Option C. The host publishes the port on
loopback only. To expose the container on all interfaces you must additionally
set `OPENSNOW_SERVER_HOST=0.0.0.0` and `OPENSNOW_ALLOW_PUBLIC=1` — see
[step 4](#4-start-the-server).)

---

## 3. Configure S3

OpenSnow registers an S3 object store at startup when an S3 **bucket** is
configured. The store is built by `register_s3` in
`crates/opensnow-core/src/engine.rs`, and the config fields live on
`EngineConfig` in the same file. There are three ways to supply the settings.

### Storage config keys

| `opensnow.toml` `[storage]` key | Env var override | Type | Meaning |
|---|---|---|---|
| `s3_bucket` | `OPENSNOW_STORAGE_S3_BUCKET` | string | Bucket name. **Required** to register the S3 store. |
| `s3_region` | `OPENSNOW_STORAGE_S3_REGION` | string | AWS region (e.g. `us-east-1`). |
| `s3_endpoint` | `OPENSNOW_STORAGE_S3_ENDPOINT` | string | Custom endpoint for MinIO/R2/etc. Omit for real AWS S3. |
| `s3_access_key` | `OPENSNOW_STORAGE_ACCESS_KEY` | string | Static access key. **Leave empty/unset** to use the credential chain. |
| `s3_secret_key` | `OPENSNOW_STORAGE_SECRET_KEY` | string | Static secret key. Pairs with `s3_access_key`. |
| `s3_allow_insecure_http` | `OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP` | bool | Allow a plaintext `http://` endpoint (private MinIO only). Default `false`. |

Sources: the TOML keys are the fields of `EngineConfig` in
`crates/opensnow-core/src/engine.rs` (the `[storage]` table deserializes into
`EngineConfig` via `OpenSnowConfig.storage` in
`crates/opensnow-core/src/config.rs`). The env-var names and their exact
spelling are the `apply_env_overrides` mappings in
`crates/opensnow-core/src/config.rs` — note the credential vars are
`OPENSNOW_STORAGE_ACCESS_KEY` / `OPENSNOW_STORAGE_SECRET_KEY` (no `S3` infix),
while bucket/region/endpoint carry the `S3` infix. Booleans accept
`1|true|yes|on` (`parse_bool` in `config.rs`).

> **Tildes:** only `warehouse_path` and the catalog `path` are tilde-expanded
> (`expand_tildes` in `config.rs`). Use absolute paths elsewhere.

### Make the S3 bucket the write warehouse (optional)

To persist `CREATE TABLE AS SELECT`, materialized-view refreshes, and trusted
`COPY INTO` outputs into S3 instead of local disk, set `warehouse_path` to an
S3 URI under the same bucket OpenSnow registers:

```toml
[storage]
warehouse_path = "s3://my-opensnow-data/warehouse"
s3_bucket      = "my-opensnow-data"
s3_region      = "us-east-1"
# credentials omitted = IRSA / instance profile / AWS env / ~/.aws/credentials
```

The startup preflight treats object-store warehouse roots differently from local
paths: it does not create a local directory for `s3://...`, and materialization
writes go through the registered object store. If `warehouse_path` names a bucket
that was not registered by `s3_bucket`, materialization fails closed with a
"no object store registered" error.

### (a) `opensnow.toml` `[storage]`

Edit the root `opensnow.toml`. For AWS with static keys (e.g. a laptop):

```toml
[storage]
warehouse_path = "~/.opensnow/warehouse"

s3_bucket     = "my-opensnow-data"
s3_region     = "us-east-1"
s3_access_key = "AKIA..."        # static keys: laptops / non-AWS hosts
s3_secret_key = "..."
```

For **keyless** AWS (EKS IRSA / EC2 instance profile), leave the keys blank —
the example config in `opensnow.toml` documents exactly this:

```toml
[storage]
s3_bucket     = "my-opensnow-warehouse"
s3_region     = "us-east-1"
s3_access_key = ""               # empty = IRSA / instance profile
s3_secret_key = ""
```

When `s3_access_key` / `s3_secret_key` are unset, the S3 client falls back to
the standard AWS credential chain. The comment on `register_s3` in
`engine.rs` states the order: **IRSA → instance profile → env vars →
`~/.aws/credentials`**. This makes the keyless config the right choice on EKS
(IRSA) and EC2 (instance profile), and it also works on a laptop that already
has `~/.aws/credentials` or `AWS_*` env vars set.

### (b) `OPENSNOW_STORAGE_*` env vars

Env vars override whatever is in the TOML file (applied *after* parsing by
`apply_env_overrides` in `config.rs`). This is the recommended way to inject
secrets in containers/Kubernetes so credentials never live in the config file:

```bash
export OPENSNOW_STORAGE_S3_BUCKET="my-opensnow-data"
export OPENSNOW_STORAGE_S3_REGION="us-east-1"
export OPENSNOW_STORAGE_ACCESS_KEY="AKIA..."     # omit for IRSA/instance profile
export OPENSNOW_STORAGE_SECRET_KEY="..."
opensnow start
```

For keyless EKS, set only the bucket/region and omit the key vars; the credential
chain (IRSA) supplies the rest:

```bash
export OPENSNOW_STORAGE_S3_BUCKET="my-opensnow-warehouse"
export OPENSNOW_STORAGE_S3_REGION="us-east-1"
opensnow start
```

> An empty-string value is treated as unset: `non_empty()` in `config.rs`
> trims and drops blank values, so `OPENSNOW_STORAGE_ACCESS_KEY=""` falls back
> to the credential chain rather than passing an empty key.

### (c) MinIO / S3-compatible via custom endpoint + insecure HTTP

For MinIO, R2, or any S3-compatible endpoint, set `s3_endpoint`. When the
endpoint starts with `http://`, OpenSnow **refuses to start the store unless you
also set `s3_allow_insecure_http = true`** — see `register_s3` in `engine.rs`,
which bails with *"refusing insecure HTTP S3 endpoint; set
s3_allow_insecure_http=true only for private MinIO demos"*. Use this only for a
private/local MinIO over plaintext.

```toml
[storage]
s3_endpoint            = "http://minio.storage.svc:9000"
s3_allow_insecure_http = true        # required for an http:// endpoint
s3_bucket              = "opensnow"
s3_access_key          = "minio-access-key"
s3_secret_key          = "minio-secret-key"
# s3_region is optional for MinIO
```

Equivalent env-var form:

```bash
export OPENSNOW_STORAGE_S3_ENDPOINT="http://minio.storage.svc:9000"
export OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP=true
export OPENSNOW_STORAGE_S3_BUCKET="opensnow"
export OPENSNOW_STORAGE_ACCESS_KEY="minio-access-key"
export OPENSNOW_STORAGE_SECRET_KEY="minio-secret-key"
```

Notes from `register_s3` (`engine.rs`):
- For a custom endpoint, virtual-hosted-style requests are **disabled**
  (path-style is used) — this is what MinIO expects. For real AWS S3 (no
  endpoint), virtual-hosted-style is enabled automatically.
- An `https://` custom endpoint does **not** require
  `s3_allow_insecure_http`; only `http://` does.

---

## 4. Start the server

```bash
opensnow start
# Web UI / REST API: http://localhost:8080
```

(From `docs/DEPLOYMENT.md` › Start. Optional flags from the `Start` command in
`crates/opensnow-cli/src/main.rs`: `--http-port`, `--pg-port`,
`--enable-pgwire`.)

OpenSnow binds to **loopback (`127.0.0.1`) by default** — see the `host`
default in `opensnow.toml` and `ServerConfig::default()` in
`crates/opensnow-core/src/config.rs`. This is the safe default and is all you
need to query your S3 data locally.

### Exposing the server (read this first)

If you set the host to a non-loopback address (`[server].host` /
`OPENSNOW_SERVER_HOST`), the server **refuses to start with auth disabled**
unless you explicitly opt in. `resolve_bind_host` in
`crates/opensnow-server/src/server.rs` bails with:

> *refusing to bind to non-loopback address … with authentication disabled.
> Enable auth by setting `OPENSNOW_JWT_SECRET`, bind to `127.0.0.1`, or set
> `OPENSNOW_ALLOW_PUBLIC=1` to explicitly accept an unauthenticated public
> listener.*

`OPENSNOW_ALLOW_PUBLIC` accepts `1|true|yes|on` (`env_allows_public` in
`server.rs`).

> **Security warning.** Exposing OpenSnow on a public interface **without auth
> and TLS is unsafe** — anyone who can reach it can register tables and run
> queries against your buckets. For any internet-exposed deployment, enable JWT
> auth (`OPENSNOW_JWT_SECRET`), terminate TLS (`[server.tls]` in
> `config.rs`, or a trusted gateway), and source secrets from a secret manager.
> See `SECURITY.md` and `docs/DEPLOYMENT.md`. `OPENSNOW_ALLOW_PUBLIC=1` is an
> explicit "I accept an unauthenticated listener" override — do not use it on
> the internet.

---

## 5. Register an S3 dataset, then query it

`COPY INTO` and other data-loading SQL is **blocked** on the public query
surface (see [troubleshooting](#troubleshooting)). The supported way to make an
existing `s3://` Parquet dataset queryable is the **register** endpoint:
`POST /api/v1/tables/register` (handler `register_table` in
`crates/opensnow-server/src/rest.rs`).

### 5a. Register the table

```bash
curl -X POST http://localhost:8080/api/v1/tables/register \
  -H 'content-type: application/json' \
  -d '{"name":"orders_ext","uri":"s3://my-opensnow-data/orders/"}'
```

- `name` must be a safe identifier — letters/digits/underscore, starting with a
  letter or underscore, ≤128 chars (`is_safe_table_name` in `rest.rs`).
- `uri` may be a single Parquet file or a **directory** of Parquet (the
  underlying DataFusion listing table accepts both — see the request docs on
  `RegisterTableRequest` in `rest.rs`).
- Accepted URI schemes (`is_supported_parquet_uri` in `rest.rs`):
  `s3://`, `gs://`, `gcs://`, `az://`, `abfs://`, `file://`, or a local path
  (`/…`, `./…`, `../…`). The URI must be ≤2048 chars and contain no newline.

Example successful response:

```json
{ "status": "ok", "table": "orders_ext", "uri": "s3://my-opensnow-data/orders/",
  "next_step": "SELECT * FROM orders_ext LIMIT 10 via /api/v1/query" }
```

> The bucket in the URI must be the bucket OpenSnow registered an object store
> for in [step 3](#3-configure-s3) (i.e. it should match `s3_bucket`). The
> object store is keyed by `s3://<bucket>` in `register_s3` (`engine.rs`).

#### When auth is enabled

`POST /api/v1/tables/register` is **admin-scoped** when JWT auth is on. In
`create_router_with_auth_and_buffer` (`rest.rs`), the table-admin routes are
wrapped with `require_admin_scope` + `jwt_required`. Obtain an admin bearer token
via `POST /auth/token` (client-credentials; see `docs/DEPLOYMENT.md`) and pass
it:

```bash
curl -X POST http://localhost:8080/api/v1/tables/register \
  -H "Authorization: Bearer <admin-access-token>" \
  -H 'content-type: application/json' \
  -d '{"name":"orders_ext","uri":"s3://my-opensnow-data/orders/"}'
```

(With auth **disabled** — the default local path — no token is needed.)

### 5b. Query it

Run a `SELECT` via `POST /api/v1/query` (handler `execute_query` in `rest.rs`):

```bash
curl -X POST http://localhost:8080/api/v1/query \
  -H 'content-type: application/json' \
  -d '{"sql":"SELECT * FROM orders_ext LIMIT 10"}'
```

Aggregations work the same way:

```bash
curl -X POST http://localhost:8080/api/v1/query \
  -H 'content-type: application/json' \
  -d '{"sql":"SELECT region, SUM(amount) AS revenue FROM orders_ext GROUP BY region ORDER BY revenue DESC"}'
```

Response shape: `{ "status": "ok", "rows": <n>, "data": "<newline-delimited JSON>" }`
(`execute_query` in `rest.rs`). When auth is enabled, `/api/v1/query` requires
the query scope (`require_query_scope`); with auth disabled it is open on the
local listener.

---

## Troubleshooting

- **"refusing insecure HTTP S3 endpoint"** — your `s3_endpoint` starts with
  `http://` but `s3_allow_insecure_http` is not set. Set
  `s3_allow_insecure_http = true` (TOML) or
  `OPENSNOW_STORAGE_ALLOW_INSECURE_HTTP=true` (env), and only for a private
  MinIO. Prefer `https://`. (`register_s3`, `engine.rs`.)

- **Credentials not found / Access Denied** — if you left keys blank, OpenSnow
  uses the chain **IRSA → instance profile → env vars → `~/.aws/credentials`**
  (comment in `register_s3`, `engine.rs`). On a laptop, run `aws configure` or
  export `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`, or set
  `OPENSNOW_STORAGE_ACCESS_KEY`/`OPENSNOW_STORAGE_SECRET_KEY`. On EKS, confirm
  the ServiceAccount's IRSA role can read the bucket. Note an empty-string key
  env var is treated as unset (`non_empty`, `config.rs`).

- **Region mismatch** — set `s3_region` / `OPENSNOW_STORAGE_S3_REGION` to the
  bucket's region. AWS rejects requests to the wrong regional endpoint.

- **S3 store didn't register** — registration only happens when `s3_bucket` is
  set, and a failure is logged as a warning rather than aborting startup
  (`build` in `engine.rs`: *"Failed to register S3 object store"*). Check the
  server logs and confirm the bucket/region/endpoint values were actually
  picked up (env vars override TOML).

- **`COPY INTO` fails / "Unsupported SQL statement"** — the public query surface
  rejects `COPY INTO` and other data-loading/DDL outside the allowed set. The
  guardrail `validate_demo_sql` in
  `crates/opensnow-server/src/sql_guardrails.rs` only permits
  `SELECT`/`WITH`/`EXPLAIN`/`SHOW`/`DESCRIBE`, `CREATE TABLE AS SELECT`,
  `CREATE/REFRESH MATERIALIZED VIEW`, and warehouse `SHOW/CREATE/ALTER/USE`
  (its test suite explicitly asserts `COPY INTO …` is rejected). **Use
  `/api/v1/tables/register` to bring S3 data in, not `COPY INTO`.**

- **I need full DDL over pgwire** — the pgwire path enforces the same demo SQL
  gate by default. An operator can opt into full SQL with `OPENSNOW_TRUSTED_SQL=1`
  on a trusted/local deployment (`trusted_sql_enabled` in
  `crates/opensnow-server/src/pg.rs`), but this lifts the guardrail and should
  never be set on an exposed listener. pgwire itself is disabled by default
  (`pg_enabled = false`; enable with `--enable-pgwire`). See
  `docs/DEPLOYMENT.md`.

---

## Current limitations

**OpenSnow can read/register S3 Parquet and can also use an object-store
`warehouse_path` for materialized outputs.** The same table materialization path
is used by `CREATE TABLE AS SELECT`, `CREATE/REFRESH MATERIALIZED VIEW`, and
trusted `COPY INTO`:

- If `[storage].warehouse_path` is a local path, OpenSnow creates the directory
  at startup and writes ZSTD-compressed Parquet under that local root.
- If `[storage].warehouse_path` starts with an object-store scheme (`s3://`,
  `gs://`, `gcs://`, `az://`, `azure://`, or `abfs://`), startup skips local
  directory creation and writes materialized Parquet through the registered
  object store (`write_warehouse_parquet` in `crates/opensnow-core/src/engine.rs`).
  For S3, that means the bucket in `warehouse_path` must match the configured
  `s3_bucket` so DataFusion has an object store registered for the URI.
- Materialized-view startup registration accepts object-store URIs directly;
  local materialized views still require the Parquet file to exist on disk.

Operational caveats:

- The public `/api/v1/query` route still rejects data-loading statements such as
  `COPY INTO`; use `/api/v1/tables/register` for existing S3 data from the public
  demo surface. `COPY INTO` over object storage is for trusted/local SQL paths
  (for example pgwire with `OPENSNOW_TRUSTED_SQL=1`) and must not be exposed on
  an unauthenticated public listener.
- Remote writes currently serialize each output Parquet file in memory before a
  single object-store `put`. This is acceptable for pilot/self-host materialized
  outputs, but large managed-service workloads still need streaming multipart
  writes, quotas, and retry/backoff before being marketed as production SaaS.
- `tenant_warehouse_path` remains a local `PathBuf` helper for tenant-local paths;
  the object-store write path uses `warehouse_uri`/`write_warehouse_parquet`
  instead.

---

## Reference

- Storage config struct: `crates/opensnow-core/src/engine.rs` (`EngineConfig`,
  `register_s3`).
- Env-var overrides & parsing: `crates/opensnow-core/src/config.rs`
  (`apply_env_overrides`, `non_empty`, `parse_bool`).
- Example `[storage]` config: `opensnow.toml` (root).
- Register endpoint & URI rules: `crates/opensnow-server/src/rest.rs`
  (`register_table`, `is_supported_parquet_uri`, `is_safe_table_name`).
- SQL guardrails (COPY INTO blocked, trusted-SQL): `sql_guardrails.rs`, `pg.rs`.
- Bind safety / `OPENSNOW_ALLOW_PUBLIC`: `crates/opensnow-server/src/server.rs`.
- Build / run / auth: `README.md`, `docs/DEPLOYMENT.md`, `SECURITY.md`.
- Reference smoke commands: `scripts/demo.sh`, `scripts/public-smoke.sh`.
