# OpenSnow -- Open-Source Analytics Warehouse

## Architecture & Technology Blueprint

> Analytics-intensive data warehouse. Deployable on localhost, Kubernetes, or cloud.
>
> "Snowflake" is a trademark of Snowflake Inc. OpenSnow is an independent
> open-source project, not affiliated with or endorsed by Snowflake Inc.;
> comparisons describe interoperability only.

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [High-Level Architecture](#2-high-level-architecture)
3. [Core Technology Decisions](#3-core-technology-decisions)
4. [Storage Layer](#4-storage-layer)
5. [Compute / Query Engine](#5-compute--query-engine)
6. [Metadata & Catalog Service](#6-metadata--catalog-service)
7. [API & SQL Frontend](#7-api--sql-frontend)
8. [Data Ingestion](#8-data-ingestion)
9. [Performance Optimization](#9-performance-optimization)
10. [Security & Governance](#10-security--governance)
11. [Kubernetes Deployment](#11-kubernetes-deployment)
12. [Developer Experience & Easy Launch](#12-developer-experience--easy-launch)
13. [Competitive Landscape](#13-competitive-landscape)
14. [Implementation Roadmap](#14-implementation-roadmap)
15. [Production Launch Plan](#15-production-launch-plan)
16. [Cross-Project Enterprise Auth and Compliance](#16-cross-project-enterprise-auth-and-compliance)

---

## 1. Executive Summary

**OpenSnow** is an open-source, analytics-intensive data warehouse inspired by Snowflake's architecture. The core thesis:

- **Compose best-in-class OSS components** -- don't build a query engine from scratch
- **Separation of storage and compute** -- object storage for data, elastic stateless compute
- **Three deployment modes** -- localhost (single binary), Kubernetes (Helm + Operator), cloud (Terraform)
- **Snowflake SQL compatibility** -- reduce migration friction
- **Time to first query < 2 minutes** -- DX is a first-class feature

### Strategy: Compose, Don't Build From Scratch

The differentiating value of Snowflake is NOT the query engine -- it's the elastic compute orchestration, unified catalog/governance, developer experience, and multi-tenancy. We build those layers and compose existing engines underneath.

---

## 2. High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                          CLIENTS                                     │
│   Tableau / Looker / dbt / Python / JDBC / REST / Web UI            │
└──────────┬──────────────────────────────┬───────────────────────────┘
           │ PostgreSQL Wire (disabled by default; trusted-local 5433 only) │ HTTPS REST (443)
           ▼                              ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      API GATEWAY (axum + pgwire)                     │
│  TLS termination · Auth (JWT/OIDC; pgwire bearer JWT) · Pooling     │
│  pg_catalog shim · Arrow Flight SQL endpoint                        │
└──────────┬──────────────────────────────────────────────────────────┘
           │
           ▼
┌─────────────────────────────────────────────────────────────────────┐
│                    SQL FRONTEND (DataFusion)                          │
│  sqlparser-rs → Analyzer → Optimizer → Distributed Physical Planner  │
│  Snowflake SQL rewrite layer · UDF registry · MV rewriting          │
└──────────┬──────────────────────────────────────────────────────────┘
           │
     ┌─────┴──────────────────────────┐
     ▼                                ▼
┌──────────────┐            ┌──────────────────────────────────────┐
│  METADATA    │            │         VIRTUAL WAREHOUSES            │
│  SERVICE     │            │                                      │
│  (Iceberg    │            │  ┌──────────┐  ┌──────────┐         │
│   REST       │            │  │ WH: ETL  │  │ WH: BI   │  ...    │
│   Catalog)   │            │  │ 4 workers│  │ 2 workers│         │
│              │            │  │ (KEDA    │  │ (scale   │         │
│  PostgreSQL  │            │  │  scaled) │  │  to zero)│         │
│  / SQLite    │            │  └────┬─────┘  └────┬─────┘         │
└──────┬───────┘            └───────┼─────────────┼───────────────┘
       │                            │             │
       │                    ┌───────┴─────────────┴───────┐
       │                    │    CACHE HIERARCHY            │
       │                    │  L1: In-Memory (moka)        │
       │                    │  L2: Local NVMe SSD (foyer)  │
       │                    └───────┬──────────────────────┘
       │                            │ cache miss
       ▼                            ▼
┌─────────────────────────────────────────────────────────────────────┐
│                      OBJECT STORAGE                                  │
│  Apache Iceberg (table format) + Apache Parquet (file format)       │
│  S3 / GCS / Azure Blob / MinIO (local/K8s)                         │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. Core Technology Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| **Language** | **Rust** (single Cargo workspace) | Memory safety, no GC pauses, single static binary, Arrow ecosystem is Rust-first |
| **Query Engine** | **Apache DataFusion** | Embeddable, extensible, Arrow-native, 900+ contributors, powers InfluxDB 3.0 |
| **Table Format** | **Apache Iceberg v2** | Vendor-neutral, hidden partitioning, time travel, REST catalog spec, broadest adoption |
| **File Format** | **Apache Parquet v2** | Arrow-native read path, universal tool support, page index + bloom filters |
| **Wire Protocol** | **PostgreSQL wire (pgwire crate)** | Unlocks every BI tool, every language driver, every connection pooler |
| **Object Store** | **`object_store` crate** (arrow-rs) | Unified trait for S3/GCS/Azure/local FS, used by DataFusion + Delta-rs |
| **Catalog Storage** | **SQLite** (local) / **PostgreSQL** (prod) | Zero-dep for dev, battle-tested for prod |
| **K8s Orchestration** | **Custom Operator** (kube-rs) + Helm | CRD-based warehouse lifecycle, scale-to-zero via KEDA |

### Key Rust Crates

| Crate | Purpose |
|-------|---------|
| `datafusion` | SQL parser, planner, optimizer, execution engine |
| `arrow` / `parquet` | In-memory columnar format / on-disk storage |
| `iceberg-rust` | Iceberg table operations |
| `object_store` | S3/GCS/Azure/local filesystem abstraction |
| `pgwire` | PostgreSQL wire protocol server |
| `arrow-flight` | Arrow Flight SQL for bulk data transfer |
| `axum` + `tonic` | HTTP REST + gRPC |
| `moka` | In-memory concurrent cache (LRU/LFU) |
| `foyer` | Hybrid memory + SSD cache |
| `sqlx` / `rusqlite` | PostgreSQL / SQLite |
| `kube-rs` | Kubernetes operator framework |
| `clap` | CLI framework |
| `tokio` | Async runtime |

---

## 4. Storage Layer

### 4.1 Columnar Storage: Parquet + Iceberg

- **Parquet v2** with ZSTD level 3 compression, dictionary encoding, page index, bloom filters
- **Row group size:** 64-128 MB compressed (one row group per file)
- **Iceberg v2** for ACID snapshots, schema evolution, hidden partitioning, time travel
- All data is **immutable write-once** files -- eliminates cache invalidation complexity

### 4.2 Object Storage Backends

| Deployment | Backend | Implementation |
|------------|---------|----------------|
| Localhost | Local filesystem | `object_store::local` |
| Localhost | MinIO (single-node) | S3-compatible via `object_store::aws` |
| Kubernetes | MinIO (distributed) | MinIO Operator, erasure-coded |
| AWS / GCP / Azure | S3 / GCS / Azure Blob | Native `object_store` implementations |

### 4.3 Data Organization

- **Micro-partitions:** Immutable Parquet files, 64-128 MB each
- **Clustering:** Configurable sort key per table, background compaction service
- **Zone maps:** Free from Parquet column stats + Iceberg manifests
- **Pruning cascade:** Iceberg partition pruning → manifest min/max → Parquet page index → bloom filters

### 4.4 Caching Hierarchy

| Tier | Technology | Contents | Size |
|------|-----------|----------|------|
| L1 (Hot) | `moka` (in-memory) | Arrow batches, Iceberg manifests, Parquet footers | 1-8 GB/node |
| L2 (Warm) | `foyer` (local NVMe SSD) | Raw Parquet column-chunk bytes | 50-500 GB/node |
| L3 (Cold) | Object Storage | All data (source of truth) | Unlimited |

**Cache invalidation:** Not needed. Iceberg data files are immutable. LRU eviction handles space.

---

## 5. Compute / Query Engine

### 5.1 Engine: Apache DataFusion

- **Vectorized columnar execution** on Arrow RecordBatches (8192 rows/batch default)
- SIMD-friendly Arrow compute kernels (AVX2/512, NEON)
- Full ANSI SQL + Snowflake compatibility rewrite layer
- Extensible: custom TableProviders, UDFs, optimizer rules, physical operators

### 5.2 Virtual Warehouses

Virtual warehouses are now a thin but real control surface around query routing and cost visibility. In local/single-node mode they are catalog-backed named compute pools: `CREATE WAREHOUSE` validates safe unquoted warehouse names plus bounded metadata (`SIZE` in `xsmall|small|medium|large|xlarge`, non-negative `MIN_NODES`/`MAX_NODES`/`AUTO_SUSPEND`, and `MAX_NODES >= MIN_NODES`) before catalog mutation. `ALTER WAREHOUSE ... RESUME|SUSPEND` gates whether non-default queries can run, `/api/v1/query` routes by the request `warehouse` field, and responses/Prometheus metrics include per-warehouse usage estimates. `MIN_NODES`, `MAX_NODES`, and `AUTO_SUSPEND` are metadata only in this local slice; they do not provision workers, enforce per-warehouse admission, or perform automatic suspend/resume. Kubernetes worker-pool isolation remains the production roadmap layer; do not claim Snowflake-equivalent multi-cluster billing or autosuspend yet.

Target architecture: each warehouse becomes an independent pool of stateless compute workers. Multiple warehouses read the same data concurrently (shared storage).

```
                   ┌─── Warehouse "analytics-xl" (KEDA 0→20) ───┐
                   │  worker-0  worker-1  worker-2  worker-3     │
                   │  8 CPU     8 CPU     8 CPU     8 CPU        │
                   │  32G RAM   32G RAM   32G RAM   32G RAM      │
                   │  100G NVMe 100G NVMe 100G NVMe 100G NVMe   │
                   └─────────────────────────────────────────────┘
                   ┌─── Warehouse "etl-medium" (KEDA 0→8) ──────┐
                   │  worker-0  worker-1                         │
                   └─────────────────────────────────────────────┘
                   ┌─── Warehouse "dashboards" (SUSPENDED) ─────┐
                   │  (0 replicas, auto-resume on query)         │
                   └─────────────────────────────────────────────┘
```

- **Current local control:** catalog-backed `CREATE WAREHOUSE`, `SHOW WAREHOUSES`, `ALTER WAREHOUSE <name> RESUME|SUSPEND`, `/api/v1/query` warehouse routing with suspended-warehouse rejection for named warehouses, and a catalog `create_enterprise_warehouse` activation gate for marketplace/BYOC provisioning that requires an active matching entitlement with `warehouse.activate` before warehouse rows are created.
- **Current cost visibility:** query responses include `warehouse`, `warehouse_size`, `duration_ms`, and `warehouse_credits_estimate`; Prometheus exposes `opensnow_warehouse_pending_queries`, `opensnow_warehouse_queries_total`, and `opensnow_warehouse_credits_estimate_total`.
- **Current safety limit:** `OPENSNOW_MAX_CONCURRENT_QUERIES` caps admitted HTTP queries process-wide (default 4, max 64) and `OPENSNOW_QUERY_TIMEOUT_SECS` caps wall time (default 30s, max 300s).
- **Roadmap scale-to-zero** via KEDA (Prometheus metric: `pending_queries`)
- **Roadmap auto-resume:** query arrives → KEDA scales to 1 → pod starts (<10s) → query dispatched
- **Roadmap workload isolation:** separate K8s resource limits per warehouse

### 5.3 Distributed Execution

Stage-based MPP (like Snowflake/Trino/Spark):

1. **Leaf stages:** Scan + filter + project (data-parallel across workers)
2. **Shuffle** via Arrow Flight (gRPC, native Arrow format)
3. **Intermediate stages:** Hash join + aggregate
4. **Final stage:** Final aggregation + sort + limit → stream to client

**Single-node mode:** Everything in-process. DataFusion partitions across CPU cores via `RepartitionExec`.

### 5.4 Snowflake SQL Compatibility

| Snowflake Feature | Implementation |
|-------------------|---------------|
| `VARIANT` / `:path` notation | Custom type + parser extension |
| `FLATTEN` / `LATERAL` | Parser + plan rewrite to `UNNEST` |
| `QUALIFY` | Subquery rewrite (window + filter) |
| `COPY INTO` | Custom stage manager + bulk loader |
| `TRY_CAST`, `IFF`, `NVL` | UDF registration |
| `DATEADD`, `DATEDIFF` | Rewrite rules |
| `CREATE OR REPLACE` | DDL handler |

### 5.5 Concurrency & Isolation

| Level | Mechanism |
|-------|-----------|
| Warehouse isolation | Separate K8s pods with resource limits |
| Resource groups (within warehouse) | Weighted fair scheduling in thread pool |
| Per-query limits | DataFusion `MemoryPool` + timeouts |
| Admission control | `OPENSNOW_MAX_CONCURRENT_QUERIES` HTTP query semaphore today; per-warehouse fair queueing remains roadmap |

---

## 6. Metadata & Catalog Service

### 6.1 Iceberg REST Catalog

Custom Rust service implementing the **Iceberg REST Catalog Spec** (`/v1/namespaces`, `/v1/tables`, etc.). Instant compatibility with Spark, Trino, DuckDB, PyIceberg, dbt.

### 6.2 Schema Evolution

Full Iceberg-native support: ADD/DROP/RENAME/REORDER columns, type widening -- all without data rewrite (field-id based mapping).

### 6.3 Time Travel & Versioning

| Snowflake Feature | OpenSnow Implementation |
|-------------------|------------------------|
| `AT (TIMESTAMP => t)` | Iceberg snapshot lookup by timestamp |
| `BEFORE (STATEMENT => id)` | Map statement ID → parent snapshot |
| `UNDROP TABLE` | Re-register metadata file in catalog |
| `CLONE TABLE` | New metadata pointing to same snapshot (zero-copy) |
| Branching (extension) | Iceberg table refs (`CREATE BRANCH`, `MERGE BRANCH`) |

### 6.4 Statistics (3-Tier)

1. **File-level** (free, in Iceberg manifests): min/max, null_count per column per file
2. **Table-level** (in catalog DB): NDV, histograms, collected via `ANALYZE TABLE`
3. **Puffin files** (Iceberg-native): Theta sketches, bloom filters for advanced optimization

### 6.5 Backend Storage

| Deployment | Backend | Rationale |
|------------|---------|-----------|
| Localhost | SQLite (WAL mode) | Zero dependencies, single file |
| K8s / Cloud | PostgreSQL | Battle-tested, managed everywhere |

The embedded Rust catalog applies idempotent SQLite migrations at startup. Empty state creates the default tenant, `opensnow.public` database/schema, default virtual warehouse, and a single-row `catalog_migrations` version record. Existing demo state is upgraded in place: legacy `query_history` and `materialized_views` tables are backfilled with `tenant_id`, and materialized-view uniqueness is enforced on `(tenant_id, name)` so migration reruns are safe.

Storage preflight runs before catalog open/upgrade. The local warehouse path is created if absent and rejected if it already points at a file, preventing catalog mutation when table data cannot be written. Runtime reset is limited to ephemeral query history and materialized-view cache metadata (`opensnow reset-runtime-state`); registered table locations and sample Parquet files remain the source of truth and must be backed up/restored together with the catalog.

---

## 7. API & SQL Frontend

### 7.1 PostgreSQL Wire Protocol (Primary)

`pgwire` crate implements PG v3 protocol. **Critical:** must implement `pg_catalog` and `information_schema` for BI tool introspection.

pgwire is disabled by default for public/external demos. When auth is disabled it remains a loopback/trusted-local compatibility endpoint only. When `OPENSNOW_JWT_SECRET`/enterprise JWT mode is configured and pgwire is explicitly enabled, startup uses PostgreSQL cleartext password auth with the OpenSnow bearer JWT as the password; the startup user must match the JWT subject and the startup database/account must match `tenant_id`. The server does not implement SCRAM-SHA-256 pgwire password auth yet, so enterprise exposure must keep bearer tokens off public cleartext links by using loopback/port-forward access or an external TLS/source-range boundary.

**Current compatibility boundary:** pgwire is simple-query only today. It is enough for `psql -c` smoke tests and basic `information_schema` probes. In auth-enabled mode it binds the authenticated subject/tenant to every query, requires `sql.query` + `table.select`, runs the same `ObjectPolicyStore::check_sql` object policy path as REST/dbt/MCP, records tenant query history, and appends catalog audit allow/deny events before execution. Python `psycopg`/`psycopg2` default extended query protocol, COPY protocol, broad `pg_catalog`, and general BI adapter introspection remain unsupported/certification gaps until the compatibility pack is expanded.

**Long-term goal:** PostgreSQL wire compatibility should unlock Tableau, Looker, Metabase, Superset, Grafana, dbt, DBeaver, DataGrip, Power BI, and language drivers (psycopg2, asyncpg, JDBC, node-postgres, pgx, sqlx) after the missing protocol/auth/catalog shims are implemented and tested.

### 7.2 Arrow Flight SQL (Bulk Data)

Secondary high-performance endpoint for bulk extract/load. DuckDB, Polars, pandas speak Flight natively.

### 7.3 REST API (Management)

`axum` serves management and demo-safe data-plane APIs under `/api/v1/`. The current implemented local/public surface includes health/status, SQL query, streaming ingest, tenant/admin/auth/dbt routes, plus two admin-scoped data movement routes:

- `POST /api/v1/tables/register` registers a local or object-store Parquet file/directory (`/path`, `file://`, `s3://`, `gs://`, `gcs://`, `az://`, `abfs://`) under a safe unqualified table name.
- `POST /api/v1/export/postgres` runs one validated OpenSnow `SELECT`/safe materialization query and writes the result to an external PostgreSQL table in `replace` or `append` mode.

When auth is enabled, both routes require `policy.admin`; in auth-disabled local demo mode they remain a trusted-local operator path and must not be exposed as a public/default endpoint.

Longer-term Snowflake-style management resources remain roadmap/API-compatibility targets:
- `/warehouses` -- create, suspend, resume, resize
- `/queries` -- async submit, poll, cancel
- `/databases`, `/users`, `/roles` -- CRUD
- `/stages`, `/copy-into` -- data loading

### 7.4 Authentication

| Method | Technology |
|--------|-----------|
| pgwire password | PostgreSQL CleartextPassword carrying an OpenSnow bearer JWT; SCRAM-SHA-256 is not implemented yet |
| REST API tokens | JWT (`jsonwebtoken` crate) |
| OAuth2 / OIDC | Account-owned OIDC IdP configuration; marketplace/BYOC release language is OIDC-only until SAML ships |
| SAML SSO | Embedded native SAML is not implemented; SAML login fails closed with `saml_unsupported_fail_closed` |
| API keys | Hashed tokens in catalog DB |

### 7.5 Web UI

**V1:** Embed CloudBeaver (Apache 2.0) -- full PG-compatible web SQL client.
**V2:** Custom Next.js + Monaco Editor + AG Grid for warehouse management + SQL editing.

---

## 8. Data Ingestion

### 8.1 Batch Loading (COPY INTO)

```sql
COPY INTO my_table FROM 's3://bucket/path/' FILE_FORMAT = (TYPE = PARQUET);
```

- OpenDAL lists files → DataFusion reads in parallel → Iceberg atomic commit
- Formats: Parquet, CSV, JSON/NDJSON, Avro
- Sources: `file://`, `s3://`, `gs://`, `abfs://`, HTTP

### 8.2 Streaming Ingestion

| Pattern | Stack |
|---------|-------|
| Simple CDC | Debezium → Kafka → Iceberg Sink Connector |
| With transforms | Debezium → Kafka → Apache Flink → Iceberg |
| Low-latency custom | Client → Arrow Flight endpoint → Iceberg |

### 8.3 Continuous Ingestion (Snowpipe-like)

```sql
CREATE PIPE my_pipe AUTO_INGEST = TRUE
AS COPY INTO target_table FROM 's3://bucket/incoming/' FILE_FORMAT = (TYPE = PARQUET);
```

Object storage notifications (S3→SQS, GCS→PubSub, MinIO→NATS) trigger micro-batch loading. File deduplication via path+ETag. Exactly-once via idempotent Iceberg commits.

### 8.4 Semi-Structured Data (VARIANT)

Shredded VARIANT type: top-level scalar fields auto-extracted into typed Arrow columns; nested/dynamic fields stored as binary JSON blob. Snowflake-compatible `:path` notation and `LATERAL FLATTEN`.

### 8.5 External Tables & Federation

```sql
-- Query files in-place
CREATE EXTERNAL TABLE web_logs STORED AS PARQUET LOCATION 's3://data-lake/logs/';

-- Cross-catalog queries
CREATE EXTERNAL CATALOG lakehouse TYPE = ICEBERG CATALOG_URI = 'http://catalog:8181';
SELECT * FROM lakehouse.db.my_table;
```

---

## 9. Performance Optimization

### Priority Tiers

**Tier 1 (Implement First -- Highest ROI):**

| Technique | Expected Impact | Implementation |
|-----------|----------------|----------------|
| Columnar compression (ZSTD + dictionary) | 5-20x storage, 3-10x I/O | Parquet write settings |
| Vectorized SIMD execution | 3-10x CPU throughput | DataFusion + Arrow kernels |
| Zone maps + bloom filters | 90-99% data skipping | Parquet stats + Iceberg manifests |
| Late materialization + predicate pushdown | 10-100x selective queries | DataFusion PruningPredicate + RowFilter |

**Tier 2 (Implement Second):**

| Technique | Expected Impact | Implementation |
|-----------|----------------|----------------|
| Result/metadata/data caching | 5-100x repeated queries | moka (L1) + foyer (L2) |
| Join optimization | 10-1000x complex queries | Hash/sort-merge/broadcast selection + reordering |
| Spill-to-disk | Reliability (prevents OOM) | DataFusion MemoryPool + FairSpillPool |

**Tier 3 (Implement Third):**

| Technique | Expected Impact | Implementation |
|-----------|----------------|----------------|
| Adaptive query execution | 2-100x star-schema | Runtime filter propagation, join switching |
| Materialized views | 100-1000x dashboards | Custom optimizer rule + incremental refresh |
| Approximate query processing | 100-1000x for ~2% error | HLL, T-Digest, sampling (DataFusion built-ins) |

**Tier 4 (Specialized):** GPU acceleration via RAPIDS cuDF -- only for compute-bound bottlenecks.

---

## 10. Security & Governance

### Design Principle: Progressive Security

- **Localhost mode:** Single-user, no auth, no TLS. Zero friction.
- **Enterprise mode:** Full auth, RBAC, encryption, audit. Production-ready.

### 10.1 Authentication

| Method | Technology |
|--------|-----------|
| Local users | Argon2id password hashing in catalog DB; local/dev only |
| OAuth2/OIDC | Per-organization IdP config, issuer/audience/JWKS validation, group-to-role mapping |
| SAML SSO | Release-gated: embedded native SAML has no metadata/ACS/assertion validation path yet, so enterprise claims are OIDC-only and login fails closed with `saml_unsupported_fail_closed` |
| SCIM | Public-platform user/group lifecycle contract; internal pipeline mode may use operator-managed membership |
| Service accounts | Scoped JWT / API keys with expiry, rotation, revocation, and audit |

OpenSnow exposes the reusable contract slice in `crates/opensnow-auth/src/contract.rs`. The types there cover: deployment modes (`Local`, `InternalPipeline`, `PublicPlatform`), IdP kinds (`local`, `oidc`, `saml`, `service_account`), SCIM lifecycle states, subject references, marketplace identity, sealed secret descriptors, policy resources/actions, and audit event envelopes.

The REST service-account adapter is wired into `/auth/token`: `OPENSNOW_CLIENTS` accepts `client_id:client_secret:primary_role[:tenant_id[:scope scope]]`, stores the secret as an Argon2 hash in memory, issues JWTs with contract-aligned `tenant_id` and `scopes`, and rejects protected REST calls whose `X-Tenant-ID` does not match the token tenant. When auth is enabled, route guards require `sql.query` + `table.select` for query/distributed-query, ingest write scopes for ingest mutations, ingest read scopes for ingest status, `policy.admin`/admin roles for admin mutations, and `audit.read`/`policy.admin` for audit export. Local/dev deployments can still use shared-secret HS256 through `OPENSNOW_JWT_SECRET`/`local_hs256`. Enterprise/BYOC deployments use the asymmetric product-token issuer (`OPENSNOW_JWT_MODE=enterprise`, RS256 or ES256) so tokens carry `iss`, `aud`, and `kid`; validation fails closed on wrong issuer/audience, unknown or revoked `kid`, and expiry. Public verification keys are published at `/auth/jwks.json` and `/.well-known/jwks.json`; rotated verify-only keys can be supplied with `OPENSNOW_JWT_VERIFICATION_KEYS_JSON` while `OPENSNOW_JWT_REVOKED_KIDS` removes compromised keys from both validation and JWKS. OIDC-derived product tokens remain distinct from service-client tokens: the callback flow persists a durable SSO session in `OPENSNOW_SSO_DB_PATH`, mints a scoped JWT carrying `auth_method=oidc` and `session_id`, and `jwt_required` re-checks that session for account/email binding, expiry, and revocation on every protected REST request.

### 10.2 Authorization

| Capability | Implementation |
|------------|---------------|
| RBAC | Built-in `GRANT/REVOKE` (Snowflake-compatible), mapped by `to_policy_action()` into shared policy actions |
| SQL policy resources | Warehouses, databases, schemas, tables, stages, integrations, and query IDs use `OpenSnowResource` adapters |
| Row-level security | Policy-injected WHERE clauses at query planning |
| Column-level masking | Dynamic masking expressions in physical plan |
| Advanced policies | **OPA** (Open Policy Agent) or future Cedar adapter behind the shared policy envelope |

### 10.3 Encryption

| Layer | Technology |
|-------|-----------|
| At-rest | AES-256-GCM envelope encryption, keys via **HashiCorp Vault** |
| In-transit | TLS 1.3 (rustls), mTLS between components (cert-manager) |
| Localhost | Local master key file, no Vault needed |

### 10.4 Audit & Governance

- **Audit tables:** `OPENSNOW.AUDIT.QUERY_HISTORY`, `LOGIN_HISTORY`, `ACCESS_HISTORY`
- **Shared audit envelope:** `AuditEventBuilder` records actor, auth method, action, resource, result, trace ID, secret-handle references, and redacted metadata without secret values.
- **Lineage:** OpenLineage events → Marquez for cross-system lineage
- **Tagging:** `ALTER TABLE SET TAG sensitivity = 'PII'` with auto-classification
- **GDPR:** Compaction-based physical deletion, configurable retention

### 10.5 Sealed secrets and marketplace identity

- Object storage credentials, external stages, catalog integrations, BI OAuth/client credentials, and encryption keys are represented as `SecretHandleDescriptor` metadata. Runtime code must resolve handles only inside trusted boundaries; list/read APIs expose metadata and handle IDs, never raw values. Current trusted boundaries include the local-dev sealed SQLite store plus production AWS Secrets Manager (`aws-secretsmanager://...`) and Vault (`vault://path#field`) resolvers that fail closed if IAM/Vault/session dependencies are unavailable; GCP Secret Manager is modeled in config/Helm values and remains a provider-handle boundary for Kubernetes ExternalSecret/workload-identity integration until a live GCP resolver smoke is added.
- Public marketplace deployments map AWS/GCP/Azure customer and entitlement IDs to an OpenSnow organization using `MarketplaceIdentity`. Entitlements feed policy decisions but do not replace SQL/RBAC authorization.
- The contract layer now includes `EntitlementCheck`, `EntitlementPlan`, `EntitlementState`, `AccountActivation`, and `WarehouseActivation` so account and warehouse activation paths can fail closed when an AWS/GCP/Azure marketplace entitlement is expired, suspended, cancelled, missing required activation features, or over the purchased warehouse limit.
- Enterprise Helm values distinguish `enterprise.mode="test-instance"` from BYOC/marketplace modes. Non-test-instance renders must use external metadata secrets, TLS for pgwire exposure, supported `enterprise.secret_provider` values (`aws-secrets-manager`, `gcp-secret-manager`, or `vault`), and marketplace entitlement IDs when entitlement gating is required; runtime config parsing re-enforces these rendered gates before startup.
- The AWS-first BYOC reference package is customer-account owned: Terraform creates private EKS worker nodes, scoped IRSA, versioned SSE-KMS S3 warehouse storage, Object-Lock audit export, optional private RDS PostgreSQL with managed Secrets Manager password and backups, and no static AWS credentials in Helm renders. The package is marketplace-ready reference material only; public listing/submission remains gated on enterprise auth/security QA.

---

## 11. Kubernetes Deployment

### 11.1 Architecture

```
┌────────── Namespace: opensnow ──────────────────────────────────────┐
│                                                                      │
│  Ingress (NGINX, TLS) → API Gateway (Deployment, HPA 2-10)         │
│                              │                                       │
│                    Query Coordinator (Deployment, HPA 2-5)           │
│                        │              │             │                 │
│                ┌───────┴──┐    ┌──────┴───┐   ┌────┴─────┐         │
│                │ WH: ETL  │    │ WH: BI   │   │ WH: Adhoc│         │
│                │StatefulSet│   │StatefulSet│   │(suspended)│        │
│                │ + PVC     │    │ + PVC    │   │ 0 replicas│        │
│                │ (cache)   │    │ (cache)  │   └──────────┘         │
│                └───────────┘    └──────────┘                         │
│                                                                      │
│  Metadata Service (StatefulSet, 3) → PostgreSQL (CloudNativePG)     │
│  OpenSnow Operator (Deployment, 1 + leader election)                │
│                                                                      │
│  External: Object Storage (S3 / GCS / MinIO)                        │
└──────────────────────────────────────────────────────────────────────┘

Monitoring NS: Prometheus + Grafana + KEDA + cert-manager
```

### 11.2 Custom Resources (Operator)

```yaml
apiVersion: opensnow.io/v1alpha1
kind: Warehouse
metadata:
  name: analytics-xl
spec:
  size: xlarge
  minReplicas: 0          # scale-to-zero
  maxReplicas: 20
  autoSuspendSeconds: 300
  autoResumeEnabled: true
  cacheStorage: { storageClass: local-nvme, size: 100Gi }
  resources: { cpu: "8", memory: "32Gi" }
```

### 11.3 Key Decisions

| Concern | Decision |
|---------|----------|
| Compute workers | **StatefulSet** (stable identity for shuffle, PVC for cache) |
| Gateway / Coordinator | **Deployment** (stateless) |
| Auto-scaling | **KEDA** on `pending_queries` metric (scale-to-zero capable) |
| Cache PVs | NVMe SSD, `reclaimPolicy: Delete` (cache is reconstructible) |
| Service mesh | **Not day one.** Use native TLS + NetworkPolicy. Add Linkerd later if needed. |
| Local dev | **k3d** (k3s in Docker) with MinIO + single-node PostgreSQL |

---

## 12. Developer Experience & Easy Launch

### 12.1 Single Binary

The `opensnow` binary contains everything: SQL engine, catalog, HTTP/Flight/gRPC servers, web UI, CLI.

```bash
opensnow start          # start full server
opensnow local 'SELECT 1'  # run a one-off query without starting the server
opensnow shell          # interactive SQL REPL
opensnow init --with-sample-data --industry=telecom   # initialize with industry sample data
opensnow status         # show health
opensnow cli contract --format json  # machine-readable OpenSnow CLI contract
opensnow cli doctor --format json    # CLI/config readiness for enterprise self-service
```

Benchmarks live in the `bench/` crate and run with `cargo bench` / `cargo run -p bench`; see `bench/README.md`.

The command-line lane is `opensnow-cli`. Its stable agent-facing schema is `OpenSnowCliReport` from `opensnow cli contract --format json`; see `docs/CLI.md`.

### 12.2 Public Demo Entry Path

The current externally testable path is repo-local and deterministic:

```bash
scripts/demo.sh
scripts/demo.sh reset
```

`scripts/demo.sh` is intentionally outside production deployment code. It writes `.opensnow-demo/opensnow.toml`, starts the CLI server on loopback when health is not already ready, loads `demo/public-demo-manifest.json` via `scripts/demo-seed.py`, runs REST smoke checks, and leaves the local server running for browser/API exploration. The manifest is synthetic, generated, and stable (`seed: 424242`) so QA and external testers can compare row counts and expected query outputs without private data or credentials.

### 12.3 Three Getting Started Flows

**Mode 1: Localhost (< 2 minutes)**
```bash
# Install
brew install opensnow/tap/opensnow    # or: curl -fsSL https://get.opensnow.dev | sh
                                       # or: pip install opensnow
                                       # or: docker run opensnow/opensnow

# Run
opensnow init --with-sample-data
opensnow start
# => Web UI at http://localhost:8080
# => REST SQL at http://localhost:8080/api/v1/query
# => Optional trusted local pgwire at localhost:5433 with --enable-pgwire
```

**Mode 2: Kubernetes**
```bash
helm repo add opensnow https://charts.opensnow.dev
helm install opensnow opensnow/opensnow \
  --set storage.type=s3 --set storage.s3.bucket=my-warehouse \
  --set worker.replicas=3
```

**Mode 3: Cloud (Terraform)**
```hcl
module "opensnow" {
  source       = "opensnow/opensnow-aws/aws"
  cluster_name = "prod"
  worker_count = 3
  s3_bucket    = "my-opensnow-warehouse"
}
```

### 12.3 Embedded Mode (Library) — _planned, not yet implemented_

> ⚠️ **Roadmap.** The embedded library APIs below are a design target. The
> `opensnow-embedded` and `opensnow-python` crates do not exist yet, there is no
> published `pip install opensnow` package, and `read_parquet(...)` is
> illustrative. Today, use the `opensnow` CLI and the server's REST/pgwire
> interfaces. The snippets are retained to document the intended API shape.

**Python** (planned — `pip install opensnow`):
```python
import opensnow
conn = opensnow.connect()
df = conn.sql("SELECT * FROM read_parquet('*.parquet') LIMIT 10").to_pandas()
```

**Rust** (planned — `opensnow-embedded` crate):
```rust
let session = opensnow_embedded::Session::builder().build().await?;
let df = session.sql("SELECT count(*) FROM read_parquet('data/*.parquet')").await?;
```

### 12.4 Configuration: Zero-Config by Default

TOML config with full env var override. `opensnow start` with NO config works: auto-detects cores, 80% RAM, local filesystem, SQLite catalog, auth disabled, web UI enabled.

### 12.5 Cargo Workspace Structure

Crates that exist today (see `Cargo.toml` for the authoritative list):

| Crate | Purpose |
|-------|---------|
| `opensnow-core` | Parser, planner, optimizer, execution (wraps DataFusion) |
| `opensnow-storage` | Object store, cache, file formats |
| `opensnow-catalog` | Iceberg REST catalog, metadata |
| `opensnow-iceberg` | Iceberg table format support |
| `opensnow-server` | HTTP, Flight SQL, gRPC, pgwire, auth |
| `opensnow-cli` | CLI binary |
| `opensnow-distributed` | Coordinator/worker distributed execution |
| `opensnow-ingest` | Streaming + batch ingestion |
| `opensnow-auth` | Enterprise auth/compliance contracts |
| `opensnow-industry` | Industry sample datasets |
| `opensnow-agent`, `opensnow-mcp` | Agent + MCP integration |
| `opensnow-queue`, `opensnow-rapids`, `opensnow-compat` | Job queue, GPU acceleration, compatibility shims |

Planned (not yet in the workspace): `opensnow-embedded` (library API), `opensnow-python` (PyO3 bindings), `opensnow-operator` (Kubernetes operator).

---

## 13. Competitive Landscape

| System | Type | Distributed | Storage/Compute Sep | SQL | Our Relationship |
|--------|------|:-----------:|:-------------------:|:---:|-----------------|
| **DuckDB** | Embedded OLAP | No | N/A | High | Optional local-mode engine |
| **ClickHouse** | OLAP DB | Yes | Partial | High | Competitor (real-time niche) |
| **StarRocks** | MPP Warehouse | Yes | Yes | High | Closest competitor; reference architecture |
| **Trino** | Query Engine | Yes | Yes (no storage) | High | Potential query layer alternative |
| **Databend** | Cloud Warehouse | Yes | Yes | Medium | Most architecturally similar; immature |
| **Spark+Iceberg** | Lakehouse | Yes | Yes | Medium | Complementary (batch ETL) |
| **Apache Doris** | MPP Warehouse | Yes | Partial | High | Competitor (China-centric community) |

**Our differentiation:** Single-binary DX, true scale-to-zero, Snowflake SQL compatibility, Iceberg-native, Rust performance, progressive security (zero-config → enterprise).

---

## 14. Implementation Roadmap

### Phase 1: Foundation (Months 1-2)
- Single Rust binary with DataFusion
- `opensnow start` / `opensnow shell` / `opensnow local`
- Local filesystem storage with Parquet + Iceberg (SQLite catalog)
- COPY INTO from local files (Parquet, CSV, JSON)
- Basic pgwire (PostgreSQL protocol) server
- Web UI (embedded CloudBeaver or minimal custom)
- Sample data (TPC-H SF1)
- Docker image, brew formula, `curl | sh` installer

### Phase 2: Object Storage + SQL Compat (Months 2-3)
- S3/GCS/Azure via `object_store`
- MinIO for local S3-compatible dev
- Snowflake SQL rewrite layer (VARIANT, FLATTEN, QUALIFY, COPY INTO)
- External tables over object storage
- Arrow Flight SQL endpoint
- Schema inference + format auto-detection

### Phase 3: Distribution + Caching (Months 3-5)
- Multi-node: Coordinator + Worker architecture
- Arrow Flight shuffle between workers
- Virtual warehouses (named compute pools)
- L1/L2 cache hierarchy (moka + foyer)
- MERGE INTO (upsert via Iceberg)
- Result caching
- Python bindings (PyO3)

### Phase 4: Kubernetes + Production (Months 5-7)
- Helm chart + Kubernetes Operator (kube-rs)
- `Warehouse` CRD with auto-suspend/resume
- KEDA auto-scaling (scale-to-zero)
- Authentication (JWT, OIDC via Dex)
- RBAC (GRANT/REVOKE)
- Encryption (TLS, at-rest)
- Audit logging
- Terraform modules (AWS, GCP, Azure)

### Phase 5: Enterprise + Ecosystem (Months 7-10)
- Streaming ingestion (Kafka + Iceberg Sink, Snowpipe-like)
- Materialized views with automatic query rewriting
- Column masking + row-level security
- Data lineage (OpenLineage)
- dbt adapter
- Airflow/Dagster/Prefect providers
- Approximate query processing
- WASM playground for docs
- Multi-tenancy

---

## 15. Production Readiness

OpenSnow is pre-1.0 software. Before any internet-exposed or multi-tenant
deployment, enable authentication and TLS, supply secrets from a managed secret
store, and review the security guidance in [`SECURITY.md`](SECURITY.md) and
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

---

## 16. Enterprise Auth and Compliance

OpenSnow's enterprise auth/compliance surface is built around stable identity and compliance contracts plus product-specific adapters: organization, SSO/OIDC/SAML, SCIM, RBAC/ABAC, audit, sealed-secret, marketplace identity, GDPR, and SOC2 requirements.

The recommended direction is shared identity/compliance contracts plus product-specific adapters first, with a path to a central control-plane service only after the contracts stabilize. OpenSnow-specific implementation work maps SQL privileges, query/audit history, warehouses, stages, object-storage credentials, row/column policies, and marketplace deployment identity into those contracts.

---

## Appendix: Technology Reference

### All Open-Source Dependencies

| Category | Technology | License | Purpose |
|----------|-----------|---------|---------|
| Query Engine | Apache DataFusion | Apache 2.0 | SQL parsing, optimization, execution |
| In-Memory Format | Apache Arrow | Apache 2.0 | Columnar data representation |
| File Format | Apache Parquet | Apache 2.0 | On-disk columnar storage |
| Table Format | Apache Iceberg | Apache 2.0 | ACID transactions, time travel, catalog |
| Object Storage | MinIO | AGPL-3.0 | S3-compatible local/K8s storage |
| PG Protocol | pgwire crate | MIT | PostgreSQL wire protocol |
| Flight SQL | arrow-flight crate | Apache 2.0 | Bulk data transfer |
| HTTP Framework | axum | MIT | REST API |
| gRPC | tonic | MIT | Internal RPC + Arrow Flight |
| Memory Cache | moka | MIT | LRU/LFU concurrent cache |
| Disk Cache | foyer | Apache 2.0 | Hybrid memory + SSD cache |
| Identity | Dex (CNCF) | Apache 2.0 | OAuth2/OIDC/SAML broker |
| Policy Engine | OPA | Apache 2.0 | Fine-grained authorization |
| Key Management | HashiCorp Vault | BUSL-1.1 | Encryption key management |
| Certificates | cert-manager | Apache 2.0 | TLS certificate issuance |
| Autoscaling | KEDA | Apache 2.0 | Event-driven pod autoscaling |
| K8s Operator | kube-rs | Apache 2.0 | Rust K8s operator framework |
| Monitoring | Prometheus + Grafana | Apache 2.0 | Metrics + dashboards |
| Lineage | OpenLineage + Marquez | Apache 2.0 | Cross-system data lineage |
| CDC | Debezium | Apache 2.0 | Change data capture |
| Streaming | Kafka / Redpanda | Apache 2.0 | Event streaming |
| Web UI (v1) | CloudBeaver | Apache 2.0 | Web SQL client |
