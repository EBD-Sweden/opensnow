# OpenSnow UI, dbt Pipeline, and Marketplace Roadmap

Date: 2026-06-01
Owner: OpenSnow product/engineering
Status: Draft product architecture from Hao's direction

## Goal

Make OpenSnow feel like an enterprise self-hosted analytics warehouse, not just a query engine demo. The next focus is the user-facing workflow: a web UI where users can query, inspect history, manage transformations, run playground experiments, promote changes to production, see lineage, and land all durable data in customer-owned object storage.

## Product thesis

OpenSnow should support two parallel modes:

1. **Playground mode**: safe, interactive, exploratory SQL/model testing. Users run manual queries, inspect results, see history, and prototype transformations without affecting production.
2. **Production workflow mode**: governed pipeline changes. Users edit models through a friendly UI, but OpenSnow stores/compiles those changes into a dbt-compatible project graph, runs jobs through the query engine, writes raw/intermediate/final layers to S3/GCS, tracks lineage, and optionally pushes final analytics tables to downstream systems.

The UI must hide unnecessary dbt complexity from business/analytics users while preserving dbt as the backend execution/lineage contract.

## Non-negotiable architecture decisions

- Customer owns data plane in enterprise/marketplace deployments.
- Object storage is the system of record:
  - raw layer
  - staging/intermediate processed layer
  - marts/final production layer
  - run artifacts/logs/manifests where appropriate
- dbt-compatible model graph is the transformation contract, even if users do not see dbt syntax/files directly.
- Every query/model run has an auditable record: actor, workspace, warehouse, SQL/model hash, source objects, target objects, timing, status, cost/bytes estimate, error class, and artifacts path.
- Lineage should come from dbt artifacts first (`manifest.json`, `run_results.json`, catalog metadata), enriched with OpenSnow query/storage metadata where useful.
- Marketplace readiness means launchable stacks in customer AWS/GCP accounts, not a shared SaaS-only demo.

## Target user flows

### 1. Query playground

User opens OpenSnow UI and can:

- select workspace / warehouse / environment
- browse schemas, tables, model outputs, and sample data
- run SQL manually
- save named queries
- rerun prior queries
- inspect result preview and download limited results
- see query status, timing, bytes/files scanned, warehouse used, and error hints
- convert an exploratory query into a draft model

### 2. Query history and audit

User/admin can see:

- their own past queries
- workspace query history subject to RBAC
- filters by status, date, warehouse, model, actor, tag, source table
- run details:
  - submitted SQL
  - normalized/rewritten SQL
  - parameters
  - results preview metadata
  - affected output path/table
  - logs and error details
  - lineage in/out

### 3. Model playground

User can create/edit a draft transformation:

- starts from SQL query or blank model
- validates syntax and references
- previews result on sampled/limited data
- compares draft output to current production output
- stores draft as a versioned object, not production
- sees dependencies and downstream impact before promotion

### 4. Production workflow

User promotes a model change:

- UI creates or updates a dbt-compatible model behind the scenes
- system validates graph and dependencies
- runs dbt compile/test/run or OpenSnow-native equivalent using dbt artifacts as contract
- writes outputs to object storage with deterministic paths
- updates catalog pointers only after successful run
- records lineage and artifacts
- can rollback to previous model/output snapshot

### 5. Data connectors / output destinations

User configures final delivery:

- keep final marts in OpenSnow/Iceberg/Parquet
- push or sync selected outputs to external analytics databases
- expose via pgwire/REST/BI connector
- optionally write to customer warehouse/database via connector jobs

Connectors should be scoped credentials with audit and least privilege.

## Data layer model

Recommended object storage layout:

```text
s3://customer-opensnow-{account}/{workspace}/
  raw/
    {source}/{table}/ingest_date=YYYY-MM-DD/*.parquet
  staging/
    {dbt_schema}/{model}/{run_id}/*.parquet
  marts/
    {dbt_schema}/{model}/snapshot={snapshot_id}/*.parquet
  artifacts/
    dbt/{job_id}/manifest.json
    dbt/{job_id}/run_results.json
    dbt/{job_id}/catalog.json
    logs/{run_id}.jsonl
  tmp/
    playground/{user_id}/{session_id}/...
```

Rules:

- raw is append-only except retention/deletion policy
- staging is run-scoped and garbage-collectable
- marts/final outputs are versioned/snapshot-addressable
- catalog pointer flips only after successful validation
- playground data has TTL/quota and never becomes production without promotion

## UI modules to build

### A. Workspace home

- health/status
- warehouses
- recent jobs
- recent queries
- data assets
- warnings/action items

### B. SQL worksheet

- editor with snippets/examples
- table/schema browser
- result grid
- future query history side panel (not present in current public demo)
- saved queries
- explain/plan view later

### C. Query history

- personal and workspace views
- detailed run page
- rerun/copy/save actions
- audit-safe redaction for secrets/credentials

### D. Model editor

- draft models
- SQL editor
- preview button
- tests/validation
- dependency impact panel
- promote to production
- rollback/version history

### E. Pipeline/jobs

- dbt graph/jobs list
- run status
- schedules/triggers
- artifacts/logs
- retry/cancel
- production deployment history

### F. Lineage graph

- native dbt DAG display
- source -> staging -> mart -> connector destination
- click node for schema, owners, freshness, last run, tests
- show current production version vs draft changes

### G. Connectors

- source connectors for ingest later
- destination connectors for final analytics layer
- credential handles only, never raw secrets after creation
- test connection and run sync

### H. Marketplace/admin setup

- object storage binding
- catalog DB status
- IdP/auth status
- audit export config
- license/entitlement status
- support bundle export

## Backend services/APIs needed

### Proposed query API (not public/current)

Current public/local server routes are limited to `POST /api/v1/query` for ad hoc execution, `GET /api/v1/status`, `POST /api/v1/demo/load`, ingest routes, and `GET /api/v1/dbt/catalog`; query history is currently an internal catalog/CLI/audit surface, not a public REST history API or browser side panel. The routes below are required future work before copy or UI can claim history list/detail/rerun support.

- `POST /api/v1/query` execute ad hoc query
- Proposed future `GET /api/v1/query-runs` list query history
- Proposed future `GET /api/v1/query-runs/{id}` details/logs/artifacts
- Proposed future `POST /api/v1/query-runs/{id}/rerun`
- `POST /api/v1/saved-queries`

### Catalog/schema API

- `GET /api/v1/catalog/schemas`
- `GET /api/v1/catalog/tables`
- `GET /api/v1/catalog/tables/{id}` schema/stats/location/freshness

### Model API

- `GET /api/v1/models`
- `POST /api/v1/models/drafts`
- `PATCH /api/v1/models/drafts/{id}`
- `POST /api/v1/models/drafts/{id}/preview`
- `POST /api/v1/models/drafts/{id}/validate`
- `POST /api/v1/models/drafts/{id}/promote`
- `POST /api/v1/models/{id}/rollback`

### Pipeline/job API

- `POST /api/v1/jobs/run`
- `GET /api/v1/jobs`
- `GET /api/v1/jobs/{id}`
- `POST /api/v1/jobs/{id}/cancel`
- `GET /api/v1/jobs/{id}/artifacts`

### Lineage API

- `GET /api/v1/lineage/graph`
- `GET /api/v1/lineage/nodes/{id}`
- `GET /api/v1/lineage/impact?model_id=...`

### Connector API

- `GET /api/v1/connectors`
- `POST /api/v1/connectors`
- `POST /api/v1/connectors/{id}/test`
- `POST /api/v1/connectors/{id}/sync`

## Metadata tables needed

Minimum production metadata expansion:

- `query_runs`
- `saved_queries`
- `model_drafts`
- `model_versions`
- `pipeline_jobs`
- `pipeline_job_steps`
- `artifact_refs`
- `lineage_nodes`
- `lineage_edges`
- `storage_objects`
- `connector_configs`
- `connector_runs`
- `audit_events`

Each table needs account/org/workspace IDs, actor IDs where applicable, lifecycle state, timestamps, trace IDs, and redacted error fields.

## dbt integration model

### Phase 1: dbt artifacts as import/display layer

- Run existing dbt project through adapter/pgwire where possible.
- Persist `manifest.json`, `run_results.json`, `catalog.json` to object storage.
- Parse artifacts into lineage tables.
- Display DAG in UI.

### Phase 2: UI-managed dbt-compatible models

- UI drafts generate dbt-compatible model files or virtual model records.
- Compile/validate with dbt-compatible semantics.
- Keep a Git-like or object-versioned model history.
- Promotion creates immutable model version + job.

### Phase 3: OpenSnow-native scheduler/executor

- Use dbt graph as input.
- Execute model DAG through OpenSnow query engine directly.
- Store artifacts in dbt-compatible format so users can export/import.
- Support schedules, manual runs, dependency-triggered runs, and rollback.

## Marketplace architecture implications

AWS marketplace stack should provision:

- EKS or ECS/Kubernetes-compatible runtime
- S3 bucket(s) with encryption/lifecycle policies
- RDS PostgreSQL for catalog/metadata
- IAM roles / IRSA for object storage access
- ALB/API endpoint + TLS
- Secrets Manager/KMS integration
- CloudWatch logs/metrics export
- optional managed Grafana dashboard

GCP marketplace stack should provision:

- GKE
- GCS buckets
- Cloud SQL PostgreSQL
- Workload Identity
- HTTPS load balancer/TLS
- Secret Manager/KMS
- Cloud Logging/Monitoring

Both must include install, upgrade, backup/restore, uninstall, smoke test, and support bundle paths.

## Priority roadmap

### P0: UI foundation and query history

- schema browser
- SQL worksheet
- query execution/history detail
- saved queries
- actor/workspace-scoped audit records
- object storage artifact references for logs/results metadata

### P1: dbt artifact ingestion and lineage display

- run/import dbt artifacts
- parse manifest/run_results/catalog
- lineage tables
- DAG UI
- model/job detail pages

### P2: Model playground

- draft model records
- preview/validate
- sample/limit execution
- compare draft vs production
- TTL/quota for playground artifacts

### P3: Production promotion workflow

- promote draft to versioned production model
- run DAG/job
- write raw/staging/marts layers to S3/GCS
- atomic catalog pointer update
- rollback

### P4: Destination connectors

- connector config/credential handles
- test connection
- sync final marts to external destinations
- audit/lineage to destination

### P5: Marketplace packaging hardening

- AWS/GCP marketplace templates
- entitlement checks
- secure defaults
- install/uninstall smoke
- release provenance/SBOM/scans

## Release gates

Do not call OpenSnow enterprise/marketplace ready until:

- query history is tenant/workspace scoped and audited
- production catalog backend and migrations are real, not just docs
- object storage layout is enforced and tested
- dbt artifact lineage is displayed and validated
- production model promotion has rollback
- marketplace stack installs from scratch and passes smoke tests
- auth/RBAC applies to UI, REST, pgwire, jobs, connectors, and lineage APIs

## Immediate next engineering slice

Build the vertical slice first, not all features horizontally:

1. Web UI SQL worksheet with schema browser.
2. Persist query runs and saved queries in catalog metadata.
3. Store query logs/artifact refs in object storage.
4. Add model draft from query.
5. Run dbt sample project, ingest artifacts, show lineage DAG.
6. Promote one draft model to production output in object storage.
7. Show query/model/job lineage end-to-end in UI.

This slice proves the product story: query -> save -> draft model -> validate -> run pipeline -> write S3 -> lineage -> history.
