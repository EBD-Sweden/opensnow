# OpenSnow control plane — CLI & MCP

OpenSnow exposes a single tool registry two ways, so both humans and LLMs can
*manage* the platform (not just query it): create/edit the dbt models that
define pipelines, run the pipeline, and change the schedule.

- **MCP server** (`opensnow mcp`) — JSON-RPC 2.0 over stdio. Point an MCP client
  (Claude Code, Claude Desktop) at it and Claude can iterate pipelines for you.
- **CLI** (`opensnow agent <tool> '<json>'`) — invoke any tool directly from a
  shell or script.

Both read the dbt project at `OPENSNOW_DBT_PROJECT_DIR`.

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
| `query`, `list_tables`, `describe_table`, `create_table` | Data access |
| `schema_introspect`, `query_history`, `migration_planner`, `refactor_test` | Schema-refactor agent tools |

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

- **Dashboards** are not yet a tool — creating Metabase dashboards needs an HTTP
  client + Metabase credentials. Today that's the `deploy/demo/metabase-*.py`
  scripts; a `dashboard_create` tool is the next addition.
- `pipeline_run` shells out to `dbt`, which must be on `PATH` or set via
  `OPENSNOW_DBT_EXECUTABLE`.
- `schedule_set` writes `opensnow_schedule.json`; the running server still reads
  `OPENSNOW_DBT_SCHEDULE_CRON/_SECS` on start — wiring the server to read the
  file directly is a small follow-up.
