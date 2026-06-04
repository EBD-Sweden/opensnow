# SQL compatibility and known limits

This page is the contract for the current external demo surface. OpenSnow executes SQL through Apache DataFusion plus a small set of OpenSnow command handlers for warehouse/catalog operations. The goal is predictable demo behavior: documented sample SQL should work end-to-end, and unsupported SQL should return a clear error instead of an opaque DataFusion planner/runtime message.

## External-demo query guardrails

`POST /api/v1/query`, `opensnow local`, `opensnow shell`, and the pgwire simple-query path apply the same SQL-shape guardrails before sending user SQL to the engine:

- One SQL statement per request. A trailing semicolon is accepted, but `SELECT 1; SELECT 2` is rejected. Run multi-step workflows as separate requests.
- Empty SQL is rejected with a sample `SELECT 1 AS smoke` hint.
- SQL text is limited to 64 KiB per request.
- Requests time out after 30 seconds by default. Set `OPENSNOW_QUERY_TIMEOUT_SECS` for trusted local testing; values are capped at 300 seconds.
- Unsupported statement families are rejected before planning with a message that points back to this document.
- Guardrail tokenization ignores SQL comments and string/quoted-identifier contents, so examples like `SELECT 'DROP TABLE text only' AS note` or `-- DROP in docs\nSELECT 1` are not rejected merely because destructive words appear in inert text.

The timeout is a REST/demo protection boundary, not a full distributed query-cancellation system. If a timeout fires while the engine worker is already inside DataFusion, the HTTP caller gets a timeout error and query history records `timeout`; deeper cooperative cancellation is a future engine feature.

## Client compatibility support matrix

Current pgwire scope is deliberately narrow and honest. It supports psql-style simple-query smoke tests and basic `information_schema` probes. In auth-disabled mode it is a loopback/trusted-local endpoint only. In auth-enabled enterprise smoke, pgwire uses PostgreSQL cleartext password auth with an OpenSnow bearer JWT as the password, binds startup `user` to the JWT subject and startup `dbname` to `tenant_id`, then applies the same scope/object-policy/audit path before execution. This is not a broad public BI compatibility claim: do not expose bearer-token pgwire on an untrusted network unless it is behind loopback/port-forward access or external TLS plus source-range controls.

| Client lane | Current status | Smoke / skip contract |
|---|---|---|
| `psql` | Supported for simple-query `SELECT`, `WITH`, `SHOW`, `DESCRIBE`, `EXPLAIN`, and documented non-destructive OpenSnow commands. In JWT mode, connect with `PGPASSWORD=<bearer token>`, `user=<JWT subject>`, and `dbname=<JWT tenant_id>`. | `scripts/public-smoke.sh` runs `psql -c "SELECT COUNT(*) AS rows ..."` when `OPENSNOW_ENABLE_PGWIRE=1`; enterprise auth smoke should use loopback/port-forward plus the JWT-bound connection fields. |
| Python `psycopg` | Documented skip/clear unsupported by default because it uses PostgreSQL extended query protocol for `cursor.execute`. | Smoke independently imports `psycopg`; accepts either a missing-package skip for this client or a clear `extended query protocol` / `docs/SQL_COMPATIBILITY.md` error. |
| Python `psycopg2` | Documented skip/clear unsupported by default for the same extended-query reason. | Smoke independently imports `psycopg2`; accepts either a missing-package skip for this client or a clear unsupported error. |
| dbt introspection | Basic REST catalog-shape endpoint is available at `/api/v1/dbt/catalog`; dbt adapter execution over pgwire is not supported yet. | Smoke checks `/api/v1/dbt/catalog` has `metadata` and `nodes`. |
| `information_schema` | Basic DataFusion-backed `information_schema.tables` and `information_schema.columns` introspection is expected for public schema tables. | Pgwire smoke runs psql probes against both views. |
| `pg_catalog` | Not a compatibility claim yet beyond clear failure for unsupported probes. A PostgreSQL-compatible `pg_catalog` shim is still required before claiming BI adapter compatibility. | Matrix documents the gap; unsupported queries must fail rather than hang. |
| BI introspection | Not supported as a general claim yet. Tools that only need simple `information_schema` probes may work locally; Tableau/Power BI/Metabase/Superset/dbt adapters are not certified. | Use documented psql and REST catalog smoke only; treat other BI probes as residual gaps for QA. |
| Extended query protocol | Unsupported; returns SQLSTATE `0A000` with a message pointing here instead of panicking/hanging. | Covered by pgwire handler unit test and Python smoke expectation. |
| COPY protocol / `COPY INTO` | Unsupported on public demo query surfaces; use `/api/v1/ingest` for sample data. | REST guardrail rejects `COPY INTO`; pgwire smoke expects `COPY ... TO STDOUT` to fail clearly. |

## Supported SQL in the demo

Core query SQL is whatever Apache DataFusion 45 supports for registered tables and files. The stable demo subset is:

```sql
SELECT 1 AS smoke;

SELECT region, COUNT(*) AS subscribers
FROM subscribers
GROUP BY region
ORDER BY subscribers DESC;

WITH revenue AS (
  SELECT region, SUM(amount) AS total_amount
  FROM public_smoke
  GROUP BY region
)
SELECT * FROM revenue ORDER BY total_amount DESC;

SHOW TABLES;
DESCRIBE public_smoke;
EXPLAIN SELECT COUNT(*) AS rows FROM public_smoke;
```

OpenSnow also handles these non-destructive command families before falling through to DataFusion:

```sql
CREATE TABLE smoke_rollup AS
SELECT region, COUNT(*) AS rows
FROM public_smoke
GROUP BY region;

SHOW WAREHOUSES;
CREATE WAREHOUSE demo WITH SIZE = 'small' MIN_NODES = 0 MAX_NODES = 1 AUTO_SUSPEND = 300;
ALTER WAREHOUSE demo SET STATE = RUNNING;
USE WAREHOUSE demo;

CREATE MATERIALIZED VIEW mv_public_smoke AS
SELECT region, COUNT(*) AS rows
FROM public_smoke
GROUP BY region;
REFRESH MATERIALIZED VIEW mv_public_smoke;
```

Warehouse names must be unquoted safe identifiers (letters, numbers, underscores; not starting with a number). Warehouse `SIZE` is restricted to `xsmall`, `small`, `medium`, `large`, and `xlarge`; node and auto-suspend values must be non-negative integers with `MAX_NODES >= MIN_NODES`. In local/single-node mode, `MIN_NODES`, `MAX_NODES`, and `AUTO_SUSPEND` are displayed metadata only and do not enforce scaling or routing isolation.

## REST sample workflow

This workflow is what external testers should run against a local or port-forwarded server.

```bash
# Health and engine status
curl -fsS http://localhost:8080/health
curl -fsS http://localhost:8080/api/v1/status

# Load a small table through the demo ingest endpoint
curl -fsS -H 'content-type: application/json' \
  -d '{"table":"public_smoke","columns":["id","region","amount"],"rows":[[1,"stockholm",10.5],[2,"gothenburg",20.0],[3,"malmo",7.5]],"replace":true}' \
  http://localhost:8080/api/v1/ingest

# Query it; trailing semicolon is accepted
curl -fsS -H 'content-type: application/json' \
  -d '{"sql":"SELECT region, COUNT(*) AS rows, SUM(amount) AS amount FROM public_smoke GROUP BY region ORDER BY region;"}' \
  http://localhost:8080/api/v1/query

# Unsupported multi-statement request: expected status=error with a clear message
curl -fsS -H 'content-type: application/json' \
  -d '{"sql":"SELECT 1; SELECT 2"}' \
  http://localhost:8080/api/v1/query

# Unsupported DML: expected status=error pointing to this known-limits page
curl -fsS -H 'content-type: application/json' \
  -d '{"sql":"DELETE FROM public_smoke WHERE id = 1"}' \
  http://localhost:8080/api/v1/query
```

## Unsupported or limited SQL

These are known limits for the current public test build:

- Multi-statement request bodies are not supported on `/api/v1/query`, `opensnow local`, `opensnow shell`, or trusted-local pgwire.
- DML (`INSERT`, `UPDATE`, `DELETE`, `MERGE`) is not supported through the external-demo query surfaces. Use `/api/v1/ingest` or `CREATE TABLE AS SELECT` for demo data loading/materialization.
- Destructive DDL (`DROP TABLE`, `DROP MATERIALIZED VIEW`) is blocked on the external-demo query surfaces; `TRUNCATE` is blocked there as well.
- Transactions (`BEGIN`, `COMMIT`, `ROLLBACK`, savepoints) are not supported.
- Stored procedures, tasks, streams, Snowflake stages, dynamic tables, and Snowflake-specific account/admin SQL are not implemented.
- `CREATE TABLE` is supported only as `CREATE TABLE ... AS SELECT/WITH ...` in the OpenSnow command layer. Column-definition DDL is not part of the stable demo path yet.
- `COPY INTO` is blocked on `/api/v1/query` for the external demo because it reads server-side paths/object-store URLs. Use `/api/v1/ingest` or trusted local CLI workflows for sample data instead.
- `CREATE TABLE ... AS ...` and `CREATE MATERIALIZED VIEW ... AS ...` targets must be single unquoted identifiers made from letters, numbers, and underscores. Schema-qualified, catalog-qualified, path-like, or quoted targets are rejected before planning.
- The query body after `AS` for `CREATE TABLE ... AS ...` and `CREATE MATERIALIZED VIEW ... AS ...` must be exactly one `SELECT` or `WITH` statement. Wrapped destructive/DML commands such as `CREATE TABLE safe AS DROP TABLE victim` are rejected before engine execution.
- SQL privilege enforcement is implemented as a lightweight object-policy slice for REST/dbt/MCP/pgwire JWT paths, but it is not planner-backed or column-complete yet. Treat unsupported SQL shapes and unrecognized object requirements as compatibility/enterprise-readiness gaps, not Snowflake-equivalent RBAC.
- Auth-enabled REST and pgwire paths bind tenant/account context to JWT claims. Auth-disabled local/demo paths remain trusted-local only and must not be used as a hosted tenant-isolation boundary.

## pgwire exposure and bearer-token safety

pgwire does not terminate TLS itself and currently asks PostgreSQL clients for `CleartextPassword`; OpenSnow interprets that password as a bearer JWT. For local smoke, `sslmode=disable` is acceptable only with `host=127.0.0.1`, a localhost tunnel, or a Kubernetes port-forward. The public hosted pgwire remains disabled by default; for any shared, hosted, or marketplace deployment, keep pgwire disabled unless an external boundary provides TLS termination, source-range restriction, and the same auth/tenant/audit configuration used by REST. Do not paste or log raw `PGPASSWORD` values in QA transcripts.

## Error contract

All REST query responses are JSON with `status`:

- Success: `{"status":"ok","rows":<row_count>,"data":"<newline-delimited JSON rows>"}`
- Guardrail/unsupported/error: `{"status":"error","message":"<human-readable explanation>"}`

Examples of expected unsupported errors:

- `OpenSnow demo accepts one SQL statement per request...`
- `Unsupported SQL statement for the external demo: DELETE... See docs/SQL_COMPATIBILITY.md.`
- `OpenSnow does not support this SQL shape yet... See docs/SQL_COMPATIBILITY.md for supported SQL and known limits.`

Keep this page updated whenever the shared demo SQL guardrail module, `/api/v1/query`, the CLI local/shell paths, the pgwire path, DataFusion version, or OpenSnow command handlers change.
