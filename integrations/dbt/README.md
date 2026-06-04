# dbt + OpenSnow

OpenSnow speaks the **PostgreSQL wire protocol**, so any dbt project can connect
to it using `dbt-postgres` — no custom adapter required for most workflows.

This directory contains:

- `profiles.yml.example` — a reference `profiles.yml` you can copy to
  `~/.dbt/profiles.yml`.
- A pointer to the experimental native Python adapter under
  [`../dbt-opensnow/`](../dbt-opensnow), if you want richer materializations.

## Quick start (PostgreSQL adapter)

1. Install dbt with the Postgres adapter:

   ```bash
   pip install dbt-core dbt-postgres
   ```

2. Start OpenSnow (PG protocol on port 5433 by default):

   ```bash
   opensnow serve
   ```

3. Copy the example profile and adjust for your project:

   ```bash
   cp profiles.yml.example ~/.dbt/profiles.yml
   ```

4. Verify the connection:

   ```bash
   dbt debug --profiles-dir ~/.dbt
   ```

5. Run your project:

   ```bash
   dbt run
   dbt test
   dbt docs generate
   ```

## Auto-discovering tables: `/api/v1/dbt/catalog`

OpenSnow exposes a dbt-shaped catalog over HTTP that mirrors the JSON
[`dbt docs generate`](https://docs.getdbt.com/reference/commands/cmd-docs)
produces. You can use this to build dashboards, lineage tools, or LLM agents
that auto-discover what tables exist without touching SQLite.

```bash
curl -s http://localhost:8080/api/v1/dbt/catalog | jq
```

Response shape:

```json
{
  "metadata": {
    "generated_at": "2026-04-29T12:34:56Z",
    "dbt_schema_version": "https://schemas.getdbt.com/dbt/catalog/v1/manifest.json"
  },
  "nodes": {
    "orders": {
      "metadata": {"type": "table", "name": "orders"},
      "columns": {
        "id":         {"type": "Int64",   "index": 0},
        "customer":   {"type": "Utf8",    "index": 1},
        "total_cents":{"type": "Int64",   "index": 2}
      }
    }
  },
  "sources": {}
}
```

The endpoint is read-only and does not require authentication.

## Notes

- OpenSnow's PG dialect is DataFusion's SQL dialect, which differs from
  Postgres in a few places. Most analytical models work as-is; if a model
  uses Postgres-only functions you may need a small `macros/` shim.
- Materializations: `view` and `table` work out of the box. `incremental`
  depends on the table sink — for now, prefer `table` (rewrites Parquet)
  over `incremental`.
- Schemas: dbt's `schema` config maps to a DataFusion schema. Default is
  `public`.
