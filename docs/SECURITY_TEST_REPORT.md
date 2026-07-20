# OpenSnow Security Test Report

**Date:** 2026-06-07
**Target:** OpenSnow current `main` build (locally compiled debug binary), HTTP API,
auth **enabled** (`OPENSNOW_JWT_SECRET`, HS256), bound to loopback. No tests were
run against any hosted public demo (intrusive scans against production are out
of scope and could disrupt it).
**Methodology:** OWASP ZAP baseline DAST + scripted two-tenant authorization
tests + manual SQL-guardrail bypass probes + a full source review of the SQL
sinks and tenant-scoping paths.

> **Tooling note — Burp Suite:** Burp was requested but is not installable here
> headlessly (Community edition has no automation API; Pro needs a licensed GUI).
> Its goals — injection and broken-access-control across the APIs — were covered
> with the equivalent automatable stack: **OWASP ZAP** (DAST), **scripted authz
> tests** (the BOLA/tenant work below), and **manual injection probes**. `sqlmap`
> was set up but is the wrong instrument for OpenSnow specifically: the data plane
> is a SQL engine *by design* (the control is an allow-list, not parameter
> hiding), so guardrail-bypass probing + the parameterized-internal-SQL review are
> the meaningful injection tests, not blind sqlmap fuzzing of a SQL endpoint.

---

## Executive summary

| Question asked | Answer |
|---|---|
| Any SQL injection? | **No exploitable SQLi found.** Catalog is fully parameterized; identifiers are allow-list validated; the public SQL surface blocks multi-statement, `COPY`, file-read functions, and all DML/DDL outside `CREATE TABLE AS SELECT`. |
| Can a user access another user's data? | **YES — confirmed live (Critical).** With auth enabled, Tenant B used its own valid JWT to read Tenant A's table, enumerate all tenants' tables, and read Tenant A's saved chart (incl. its SQL). |
| OWASP ZAP DAST | **0 failures, 10 warnings** — all missing HTTP security headers + minor info items. No injection/XSS surfaced passively. |

The authentication gateway is solid (unauthenticated → 401; JWT tenant-spoofing →
403).

**Deployment model (clarified 2026-06-08):** OpenSnow is **not** sold multi-tenant.
Each customer gets its own instance (Snowflake-account style: one account, its own
users/roles); different corporates never share an instance. The only shared-instance
case is the **public demo**, where different visitors log in under different orgs.
This reframes the findings:

- **The cross-tenant finding (#1) does not apply to the sold product** — a
  single-customer instance has no other tenant's data to leak.
- **What matters for the product is intra-account RBAC**: per-role table grants must
  hold on *every* surface. They were enforced on pgwire but **not** on the REST
  query API — that gap is the real residual finding and is now **fixed** (see
  Remediation).
- **The demo** keeps the shared-sandbox behavior by choice; a disclaimer was added.

---

## Findings

| # | Severity | Finding | Status |
|---|---|---|---|
| 1 | **Critical** | Cross-tenant data read — all tenants share one DataFusion session / `public` schema | Confirmed live |
| 2 | High | Cross-tenant table create/overwrite in the shared namespace | Confirmed (code + live create) |
| 3 | Medium | `SHOW TABLES` / `information_schema` enumerate every tenant's tables | Confirmed live |
| 4 | Medium | Chart store is global — any tenant lists/reads/deletes another tenant's charts + SQL | Confirmed live |
| 5 | Low | Missing HTTP security headers (CSP, X-Frame-Options, X-Content-Type-Options, Permissions-Policy, COEP) | Confirmed (ZAP) |
| — | Pass | SQL injection (catalog, identifiers, guardrail bypass, file read) | No issue found |

---

### 1. Critical — cross-tenant data access (BOLA / missing data isolation)

**Proof (live, two authenticated tenants, auth enabled):**

```
1. Tenant A (JWT tenant_id=tenantA): POST /api/v1/query
   {"sql":"CREATE TABLE a_secret AS SELECT 'A-PRIVATE-topsecret' AS secret, 999 AS amt"}
   -> 200 {"status":"CREATED","rows_created":1}

2. Tenant B (JWT tenant_id=tenantB, different token): POST /api/v1/query
   {"sql":"SELECT * FROM a_secret"}
   -> 200 {"secret":"A-PRIVATE-topsecret","amt":999}      <-- B READ A'S DATA

3. Tenant B: {"sql":"SHOW TABLES"}                         -> lists a_secret
   Tenant B: {"sql":"SELECT table_name FROM information_schema.tables"} -> lists a_secret
```

**Root cause (confirmed in code):** the engine holds a **single** DataFusion
`SessionContext` built once with `.with_default_catalog_and_schema("opensnow",
"public")` (`crates/opensnow-core/src/engine.rs`). `EngineHandle::execute_sql`
takes only the SQL string — there is no tenant parameter
(`crates/opensnow-core/src/engine_handle.rs`). The REST handler extracts the
caller's `TenantId` but uses it **only** to record query history
(`crates/opensnow-server/src/rest.rs` `execute_query`); the actual execution runs
on the shared session. CTAS/materialized-view/COPY always write to the shared
`opensnow/public/<name>.parquet`. `tenant_warehouse_path` exists but the query
path never uses it.

**Fix direction:** thread `tenant_id` into query execution and isolate per tenant
— either a per-tenant `SessionContext`, or a per-tenant DataFusion
`SchemaProvider`/catalog so a tenant can only resolve its own tables (this also
fixes findings 2 and 3 and hides other tenants from `information_schema`). Add a
regression test: tenant B querying tenant A's table must return *table not found*.

### 2. High — cross-tenant table creation in shared namespace
Because the namespace is global, any tenant can `CREATE TABLE <name>` that
collides with / overwrites another tenant's table and registers it into the shared
session. Same fix as #1 (write under `opensnow/<tenant>/…` and register into the
tenant's schema).

### 3. Medium — metadata enumeration across tenants
`SHOW TABLES`, `information_schema.tables`, and `pg_catalog` enumerate every
tenant's tables (`with_information_schema(true)` on the shared session). Resolved
by the per-tenant catalog in #1.

### 4. Medium — global chart store
**Proof:** Tenant A saved a chart via `POST /api/v1/charts`; Tenant B's
`GET /api/v1/charts` returned it, including the embedded SQL
(`{"title":"A secret chart","sql":"SELECT * FROM a_secret"}`). The store
(`crates/opensnow-server/src/charts.rs`) is a single global file; reads are
unfiltered and delete/overwrite are not ownership-checked. **Fix:** store and
filter chart records by `tenant_id`; enforce ownership on delete/overwrite.

### 5. Low — missing HTTP security headers (ZAP baseline)
ZAP reported 0 failures, 10 warnings — defense-in-depth header gaps on the HTTP
responses: **Content-Security-Policy** not set, **X-Frame-Options**
(anti-clickjacking) missing, **X-Content-Type-Options** missing,
**Permissions-Policy** not set, **Cross-Origin-Embedder-Policy** missing; plus
cacheable content and a passive "SQL in response body" heuristic (the console
embeds example SQL — low concern). Also flagged: `vendor/vega.min.js` uses
eval-like functions (third-party lib). **Fix:** add a response-header middleware
(axum `SetResponseHeaderLayer`) setting CSP, `X-Frame-Options: DENY`,
`X-Content-Type-Options: nosniff`, `Referrer-Policy`, and `Permissions-Policy`.

---

## What is working (verified)

- **AuthN gate:** unauthenticated `POST /api/v1/query` → **401**.
- **Tenant-spoofing rejected:** Tenant A's JWT + `X-Tenant-ID: tenantB` → **403**
  (`jwt_required` binds the request tenant to the JWT claim).
- **No SQL injection:** every probe rejected — multi-statement (`SELECT 1; DROP…`),
  stacked CTAS, comment-evasion, `DELETE`, `COPY INTO … FROM '/etc/passwd'`
  (unsupported on the public surface), `read_parquet('/etc/passwd')` (function not
  registered), and `FROM '/etc/passwd'` (treated as a missing table, no file read).
- **Parameterized data layer:** the SQLite catalog uses bound parameters
  throughout; output identifiers go through allow-list validation
  (`validate_output_table_identifier` / `is_safe_unqualified_identifier`).
- **`UNION`/`CTE`/`information_schema` are *allowed*** by the guardrail (they are
  legitimate SELECTs) — harmless for injection, but they are the mechanism behind
  finding #1, so isolation must be enforced at the catalog level, not by SQL
  string filtering.

---

## Remediation priority

1. **Critical/High/Med (1–4):** implement per-tenant data isolation (per-tenant
   schema/catalog + tenant-scoped writes + tenant-scoped chart store). One
   architectural change closes 1–4. Gate any multi-tenant offering on it.
2. **Low (5):** add the security-header middleware (quick win, ~1 file).

Re-run this suite (ZAP baseline + the two-tenant authz script + guardrail probes)
after the fix; finding #1's regression test is the acceptance gate.

---

## Remediation applied (2026-06-08)

Given the single-tenant deployment model, work focused on the controls that matter
for it. Verified live against a rebuilt local auth-enabled instance.

**1. REST query API now enforces the role-based object policy (RBAC parity).**
`POST /api/v1/query` (`crates/opensnow-server/src/rest.rs` `execute_query`) now runs
the same `ObjectPolicyStore.check_sql` authorization the pgwire surface uses, before
executing. `jwt_required` already attaches the caller's `AuthContext` + policy store
as request extensions; the handler now consults them. Behavior:
- **No-op when auth is disabled** (local/demo mode — extensions absent), so the
  public demo is unchanged.
- **Deny-by-default for non-admin roles** (matches pgwire and Snowflake): a role
  needs an explicit `SELECT` grant on a table; platform admins
  (`ACCOUNTADMIN`/`SYSADMIN`, or `policy.admin`/`admin`/`*` scope) bypass.
- Live proof: admin `CREATE TABLE secret_t …` → ok; non-admin (`role=analyst`,
  scopes `sql.query`,`table.select`, no grant) `SELECT * FROM secret_t` →
  `object policy denied: role lacks SELECT on TABLE secret_t`; non-admin `SELECT 1`
  → ok; admin read → ok.
- **Operational note:** after this change, auth-enabled deployments are
  deny-by-default — roles must be granted table access (as on pgwire). `SHOW TABLES`
  / `information_schema` introspection is also denied for non-admin roles (parity
  with pgwire); non-admins discover objects via the role-filtered dbt/catalog views.

**2. Security headers added.** A response middleware
(`add_security_headers` in `rest.rs`) now sets `Content-Security-Policy`,
`X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy`,
`Permissions-Policy`, and `Cross-Origin-Opener-Policy` on every response. Verified
present via `curl -I`. (CSP allows inline + `unsafe-eval` because the console uses
inline scripts and Vega-Lite uses `Function`/eval; tightening to nonces is a
follow-up.)

**3. Demo disclaimer added.** The console footer now states it is a shared public
sandbox and warns against pasting private data (the demo's shared-instance
visibility is retained by choice).

**Not changed (by design):** multi-tenant data isolation in a single shared
instance — out of scope because the product is sold single-tenant. If a true
multi-tenant SaaS is ever pursued, the per-tenant catalog work in
`docs/MANAGED_SERVICE_READINESS.md` is the prerequisite.
