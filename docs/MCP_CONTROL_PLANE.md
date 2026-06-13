# OpenSnow control plane — CLI & MCP

OpenSnow exposes a single tool registry three ways, so both humans and LLMs can
*manage* the platform (not just query it): create/edit the dbt models that
define pipelines, run the pipeline, change the schedule, and build dashboards.

- **MCP server, stdio** (`opensnow mcp`) — JSON-RPC 2.0 over stdio. Point an MCP
  client (Claude Code, Claude Desktop) at it and Claude can iterate pipelines
  for you.
- **MCP server, remote HTTP** (`POST /mcp` on the `opensnow-mcp` HTTP server,
  port 8090) — the same JSON-RPC handler over streamable HTTP, behind the
  server's bearer-token / JWT auth. This is the endpoint remote clients
  (ChatGPT connectors/apps, Claude remote MCP) connect to.
- **CLI** (`opensnow agent <tool> '<json>'`) — invoke any tool directly from a
  shell or script.

All read the dbt project at `OPENSNOW_DBT_PROJECT_DIR`. Every tool carries MCP
annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`,
`openWorldHint`, `title`) so clients can distinguish retrieval from
write/delete actions — a requirement for ChatGPT app submission (see
`CHATGPT_APP_ALIGNMENT.md`).

## Tools

| Tool | Purpose |
|---|---|
| `dbt_list_models` | List every model (pipeline step) + layer |
| `dbt_get_model` `{name}` | Read a model's SQL |
| `dbt_write_model` `{name, sql, layer?}` | Create/overwrite a model |
| `dbt_delete_model` `{name}` | Delete a model |
| `pipeline_run` `{select?}` | Build models in dependency order (dbt run) |
| `pipeline_status` | DAG + last-run status from dbt artifacts |
| `schedule_get` / `schedule_set` `{cron?\|interval_secs?}` | Read/update the schedule |
| `dashboard_list` | List Metabase dashboards + public URLs |
| `dashboard_create` `{name, cards:[{title, sql, display, dimensions, metrics}]}` | Build & publish a Metabase dashboard, returns public URL |
| `chart_list` / `chart_create` | List/create native Vega-Lite charts (OpenSnow Build board) |
| `query`, `list_tables`, `describe_table`, `create_table`, `suggest_schema` | Data access & schema design |
| `schema_introspect`, `query_history`, `migration_planner`, `refactor_test` | Schema-refactor agent tools (read-only) |
| `analytics_schema_refactor` `{tables?}` | Run the **full schema-refactor agent** in one call: introspect → rank hot tables from query history → propose CTAS migration plans → smoke-test. Read-only report; pair with `schedule_set` or an external scheduler to run it periodically |
| `warehouse_list` / `warehouse_create` `{name, size?, min_nodes?, max_nodes?, auto_suspend_secs?}` | List / create virtual warehouses (compute) |
| `register_table` `{name, uri}` | Register an external Parquet file as a queryable table (loads data not expressible via SQL alone) |
| `table_drop` `{name, if_exists?}` | Drop a table (idempotent by default) |
| `materialized_view_create` `{name, sql}` / `_refresh` `{name}` / `_drop` `{name}` | Materialized-view lifecycle |

**Coverage note:** any SQL-expressible operation (e.g. `CREATE WAREHOUSE`,
`COPY INTO`, `CREATE MATERIALIZED VIEW`, `DROP`) is also reachable directly via
the `query` tool; the structured tools above exist for discoverability, safe
identifier handling, and per-tool authorization.

The dashboard tools need Metabase credentials in the environment:
`METABASE_URL` (default `https://metabase.ebdsweden.com`), `MB_USER`, `MB_PASSWORD`.

## Authentication modes

The remote `/mcp` endpoint accepts, in precedence order, whichever modes are
configured — so an organization picks what fits their stack:

| Mode | Enable with | Notes |
|---|---|---|
| External OAuth 2.x / OIDC | `MCP_OIDC_ISSUER` (+ optional `MCP_OIDC_AUDIENCE`, `MCP_OIDC_JWKS_URL`, `MCP_OIDC_JWKS_TTL_SECS`, `MCP_OIDC_DEFAULT_ROLE`) | Validates tokens issued by the org's own IdP (Okta, Entra ID, Auth0, Keycloak, Google…) via OIDC discovery + JWKS. Asymmetric algs only (RS256/384/512, ES256). Scopes/roles map from `scope`/`scp`/`scopes` and `roles`/`role`/`groups`. |
| Shared-secret JWT (HS256) | `MCP_JWT_SECRET` | OpenSnow-issued tokens (`/auth/token`). |
| Static bearer token | `MCP_AUTH_TOKEN`, or `MCP_TOKEN_<ROLE>=<token>` | Coarse; no per-tool scope RBAC. |
| None (dev/demo) | _unset_ | Open; used by the public demo. |

In the JWT and OIDC modes, per-tool RBAC applies: read tools need a read scope
(`sql.query`/`table.select`/`mcp.read`); writes need a control scope
(`table.create`, `pipeline.admin`, `dashboard.admin`) or an admin role
(`ACCOUNTADMIN`/`SYSADMIN`, scope `policy.admin`/`admin`/`*`).

## Connect Claude (MCP)

`.mcp.json` (Claude Code) or the Claude Desktop config:

```json
{
  "mcpServers": {
    "opensnow": {
      "command": "/abs/path/to/opensnow",
      "args": ["mcp"],
      "env": {
        "OPENSNOW_DBT_PROJECT_DIR": "/abs/path/to/deploy/demo/dbt",
        "OPENSNOW_DBT_EXECUTABLE": "/abs/path/to/.venv-dbt/bin/dbt",
        "OPENSNOW_OTEL_DISABLED": "1"
      }
    }
  }
}
```

Then ask Claude things like *"add a staging model for interest rates and a mart
joining it to house prices, then run the pipeline"* — it will call
`dbt_write_model` + `pipeline_run` and report status.

## Connect a remote LLM (ChatGPT, claude.ai) over HTTP

Run the HTTP server and point the client at `/mcp`:

```bash
# Bearer-token auth (single shared token):
MCP_AUTH_TOKEN=<secret> ./opensnow-mcp        # listens on :8090

# Or JWT auth:
MCP_JWT_SECRET=<secret> ./opensnow-mcp
```

```bash
curl -s -X POST https://your-host:8090/mcp \
  -H 'Authorization: Bearer <token>' \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'
```

In ChatGPT: *Settings → Connectors → Add custom connector* (developer mode)
with the `/mcp` URL. JSON-RPC notifications receive `202 Accepted` with no
body; SSE streaming is not yet implemented (`GET /mcp` returns 405, which the
streamable-HTTP spec permits).

> **Scope note:** `/mcp` is authenticated but not yet object-policy scoped —
> any valid token can call any tool, including write tools. Use a dedicated
> token and treat it as admin-level. Fine-grained per-tool RBAC is on the
> ChatGPT-app gap list (`CHATGPT_APP_ALIGNMENT.md`).

## Use from the shell (CLI)

```bash
export OPENSNOW_DBT_PROJECT_DIR=$PWD/deploy/demo/dbt
opensnow agent dbt_list_models
opensnow agent dbt_get_model '{"name":"stg_gdp"}'
opensnow agent dbt_write_model '{"name":"mart_demo","sql":"select 1 as x","layer":"marts"}'
opensnow agent pipeline_run
opensnow agent schedule_set '{"cron":"0 6 * * *"}'
```

## Notes / not yet done

- `pipeline_run` shells out to `dbt`, which must be on `PATH` or set via
  `OPENSNOW_DBT_EXECUTABLE`.
- `schedule_set` writes `opensnow_schedule.json`; the running server still reads
  `OPENSNOW_DBT_SCHEDULE_CRON/_SECS` on start — wiring the server to read the
  file directly is a small follow-up.
