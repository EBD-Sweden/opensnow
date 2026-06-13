# ChatGPT App Alignment — OpenSnow

Goal: ship OpenSnow as a ChatGPT app ("connect your LLM, manage the whole
platform, check results in OpenSnow"). This document maps OpenAI's
requirements to our current state and lists the remaining gaps.

Sources reviewed (2026-06-12):

- [App submission guidelines (Apps SDK)](https://developers.openai.com/apps-sdk/app-submission-guidelines)
- [App Developer Terms](https://openai.com/policies/developer-apps-terms/)
  *(page is bot-gated; reviewed via secondary summaries — re-verify the primary
  text in a browser before submission)*

## Where we stand

| Requirement (OpenAI) | Status | Notes |
|---|---|---|
| Remote MCP server (not stdio) | ✅ basic | `POST /mcp` on the `opensnow-mcp` HTTP server reuses the stdio JSON-RPC handler. SSE leg not implemented (`GET /mcp` → 405, permitted by the streamable-HTTP spec). |
| Tool annotations: `readOnlyHint` / `destructiveHint` / `openWorldHint` correctly designated | ✅ | All 22 tools annotated in `tools/list` (`crates/opensnow-agent/src/mcp.rs`), with tests asserting presence. `query` is honestly marked write-capable/destructive since it accepts DDL. Metabase tools are `openWorldHint: true` (external system). |
| Tool names human-readable, descriptions match behavior | ✅ | Names are specific (`dbt_write_model`, `pipeline_run`, …); descriptions state side effects ("Overwrites any existing schedule", "no DDL is applied"). |
| Inputs minimal and purpose-driven | ✅ | Each tool takes only what it acts on (SQL text, model name, cron expr). No location, no contact data, no free-form user profiling fields. |
| Functionality beyond ChatGPT's native capability | ✅ | Warehouse SQL over Iceberg/DataFusion, dbt pipeline management, scheduled refactor agent, published dashboards — all server-side state ChatGPT cannot do natively. |
| Predictable/reliable behavior, tested | ✅ | MCP handler + HTTP endpoint covered by unit tests (`opensnow-agent` mcp tests, `opensnow-mcp` router tests). Run the QA checklist before submission. |
| Restricted data (PCI, PHI, SSN, credentials) not collected | ✅ | Tools never ask for credentials or personal identifiers; Metabase/JWT secrets come from server-side env, never through tool inputs. Demo datasets are public macro statistics (Eurostat/BIS/SSB). |
| No full-chat-log reconstruction; return only relevant data | ✅/⚠️ | Tools are request-scoped. ⚠️ `query_history` returns SQL text + user/duration metadata — acceptable for the platform-admin use case, but review before submission and trim fields not needed by the calling context (see gap 5). |
| Authentication transparent; test credentials with sample demo account | ⚠️ | Bearer/JWT auth works today (fine for ChatGPT *developer-mode connectors*). Published apps need **OAuth 2.1 + dynamic client registration** (gap 1). Demo account exists: `opensnow init --with-sample-data` provisions a fully-featured sample warehouse. |
| Per-tool authorization (read vs write) | ✅ | `/mcp` now maps each tool to a required JWT scope (`authorize_mcp_tool` in `opensnow-mcp/src/lib.rs`): read tools need a read scope, write/control tools need admin or a control scope, and the `query` tool reuses object-level SQL analysis. Enforced only in JWT mode (demo stays auth-off). Tested. |
| Privacy policy (categories, purposes, recipients, retention, user controls) | ✅ draft | Drafted at `docs/PRIVACY_POLICY.md`; **must be hosted at a public URL and legally reviewed** before submission (gap 3). |
| Data minimization in outputs | ✅ | `query_history` omits per-user attribution + internal trace id by default (`include_user: true` to opt in). Tool outputs carry no IPs / diagnostic telemetry. |
| Support contact, developer verification, Platform Dashboard submission | ❌ | Organizational, not code (gap 4). |
| No ads, no unrelated content insertion | ✅ | N/A — pure tooling. |
| Prohibited categories (gambling, weapons, unregulated financial services, …) | ✅ | OpenSnow is BI/warehouse tooling. It *analyzes* financial statistics but provides no financial service, advice, or transactions. |
| Commerce rules (physical goods only, external checkout) | ✅ | N/A — no in-app commerce. |
| Embedded UI / `frameDomains` disclosure | ⚠️ | Only relevant if we ship an Apps SDK component that iframes Metabase dashboards (gap 6). Returning the public dashboard URL as text needs no disclosure. |
| Data from App Requests used only to serve the request / legal compliance | ✅ | Tool calls are executed and results returned; query history retention is disclosed in `PRIVACY_POLICY.md`. |
| Users can disconnect at any time | ✅ | Token-based: revoke the token / delete the connector and access ends. OAuth (gap 1) makes this first-class. |

## Gap list (ordered)

1. **OAuth 2.1 for `/mcp`** — required for a published app (bearer tokens are
   only accepted for developer-mode connectors). Needs: authorization-code +
   PKCE flow, dynamic client registration, token issuance bound to the
   existing JWT scopes so object policy applies.
2. ~~Per-tool authorization on `/mcp`~~ — **DONE** (`authorize_mcp_tool`):
   `sql.query`/`table.select`/`mcp.read` → read tools; `table.create` →
   `create_table`; `pipeline.admin` → dbt/pipeline/schedule writes;
   `dashboard.admin` → Metabase/chart writes; `query` → object-level SQL
   analysis. Admins bypass. Only enforced in JWT mode.
3. **Privacy policy** — drafted at `docs/PRIVACY_POLICY.md`. **Remaining:**
   host at a stable URL (e.g. `opensnow.ebdsweden.com/privacy`) and run a
   legal/GDPR review before referencing it in the submission.
4. **Submission logistics** — OpenAI Platform Dashboard (Owner role /
   `api.apps.write`), developer verification, support contact, screenshots
   that match real behavior, demo credentials pointing at an
   `init --with-sample-data` instance.
5. ~~Data-minimization pass on outputs~~ — **DONE**: `query_history` drops the
   per-user identifier and internal trace id by default (`include_user: true`
   to opt in); other tool outputs already carry no trace IDs / diagnostic
   metadata.
6. **(Optional) Apps SDK UI component** — an inline dashboard card rendered in
   ChatGPT. If it iframes `metabase.ebdsweden.com`, declare it via
   `frameDomains` and expect extended review. Text-URL responses are the
   zero-risk default until then.
7. **SSE leg of streamable HTTP** — long tools (`pipeline_run`,
   `analytics_schema_refactor`) currently block the POST until done; fine for
   demo-scale, but streaming avoids client timeouts on big projects.

## What changed (2026-06-12 → 06-13)

Pass 1 (06-12):
- `analytics_schema_refactor` exposed as an MCP tool (was CLI/HTTP-task only)
  — the full tool registry is now reachable from any connected LLM.
- MCP annotations added to every tool; tests enforce that no tool ships
  without them.
- `ping` + `notifications/initialized` handled per spec (remote clients send
  these; notifications get no JSON-RPC response, HTTP returns 202).
- `POST /mcp` JSON-RPC endpoint added to the authenticated HTTP server —
  first step to a remote MCP server, usable today as a ChatGPT developer-mode
  connector.
- `opensnow agent analytics_schema_refactor` CLI parity fixed.

Pass 2 (06-13):
- **Per-tool RBAC on `/mcp`** (gap 2) — `authorize_mcp_tool` gates each tool by
  JWT scope; read tokens can't invoke write/control tools. 4 new tests.
- **Data minimization** (gap 5) — `query_history` omits user identifier +
  trace id by default. Verified the schema-refactor agent (which reads only
  SQL text) is unaffected.
- **Privacy policy draft** (gap 3) — `docs/PRIVACY_POLICY.md`; needs hosting +
  legal review.

## Remaining before a published-app submission

1. OAuth 2.1 + dynamic client registration on `/mcp` (gap 1) — the gatekeeper.
2. Host + legally review the privacy policy (gap 3).
3. Submission logistics (gap 4) — org/dashboard work, not code.
4. Optional: embedded UI component (gap 6), SSE streaming (gap 7).
