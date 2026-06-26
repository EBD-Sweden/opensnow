# OpenSnow — Privacy Policy

**Last updated:** 2026-06-13
**Operator:** EBD Sweden ("we", "us")
**Contact:** hao.huang@ebdsweden.com

This policy explains what data an OpenSnow app/connector processes when you
connect it to an LLM client (for example, a ChatGPT developer-mode connector or
Claude), and how the self-hosted OpenSnow instance handles it. It is a reviewed
repository policy for self-hosted/customer-hosted deployments, not evidence that
the public demo currently hosts a privacy page or a public-directory app.

## 1. What OpenSnow is

OpenSnow is a self-hosted analytics data warehouse and pipeline control plane.
When you connect an LLM client to an OpenSnow server over MCP, the model can run
SQL, manage dbt models, run the data pipeline, and build dashboards **on the
OpenSnow instance you point it at**. We do not operate a shared multi-tenant
service that ingests your data on our servers; you (or your organization) run
the OpenSnow instance and own the data in it.

## 2. Data we process

When a tool is invoked, OpenSnow processes only what the request requires:

| Category | Examples | Why |
|---|---|---|
| Tool inputs | SQL text, dbt model names and SQL, schedule expressions, dashboard/chart definitions | To execute the action you asked for |
| Warehouse data | Rows in the tables you have loaded into your OpenSnow instance | Returned as query results, only for the query you run |
| Query history | The SQL text, timing, and row-count metrics of past queries | Powers usage analysis and the schema-refactor agent |
| Pipeline metadata | dbt model sources, run status, schedule config | To manage and report on your pipeline |

We do **not** ask for or collect, through tool inputs: passwords, API keys, MFA
codes, payment-card data, government identifiers, or protected health
information. Server credentials (Metabase login, JWT secret) are read from the
OpenSnow server's environment and are never transmitted through tool inputs or
returned in responses.

By default, the `query_history` tool **omits per-user attribution and internal
trace IDs** — it returns only SQL and cost/row metrics. User attribution is
included only when a caller explicitly opts in (`include_user: true`).

## 3. What we do not do

- We do not reconstruct, infer, or store your LLM conversation history.
- We do not use your data to train models.
- We do not sell your data or use it for advertising or behavioral profiling.
- Responses return only data relevant to the request; we do not attach IP
  addresses or diagnostic telemetry to tool outputs.

## 4. Recipients

Data stays on your OpenSnow instance except in one case you control: when you
ask OpenSnow to **publish a dashboard**, the relevant query results and chart
definitions are sent to the Metabase instance you have configured
(`METABASE_URL`), which may render a publicly shareable URL. Only invoke
dashboard tools with data you intend to publish.

## 5. Retention and your controls

- **Query history** is retained in your instance's catalog until you clear it.
  Run `opensnow reset-runtime-state` to clear query history and materialized-view
  cache metadata (registered tables and data files are preserved).
- **Warehouse data, dbt models, dashboards** persist until you delete them
  (e.g. `dbt_delete_model`, dropping a table, deleting a dashboard).
- **Disconnecting:** you can disconnect the OpenSnow connector from your LLM
  client at any time; access ends immediately. Revoking the bearer/OAuth token
  used by the connector also terminates access.

## 6. Security

Remote MCP access (`/mcp`) is authenticated (bearer token or JWT) and, in JWT
mode, authorized per tool by scope — read-only tokens cannot invoke write or
control tools. Object-level SQL privileges are enforced when an object policy is
configured. Run the server over TLS and issue a dedicated, least-privilege token
per connector.

## 7. The demo instance

The public demo (`opensnow.ebdsweden.com`) uses only **synthetic, deterministic
sample data** (`demo/public-demo-manifest.json`, `contains_real_customer_data:
false`). No real personal or customer data is present in the demo.

## 8. Changes

We will update the "Last updated" date above when this policy changes and, for
material changes, surface a notice through the app listing.
