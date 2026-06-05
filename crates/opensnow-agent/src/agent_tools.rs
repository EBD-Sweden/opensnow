use std::time::Instant;

use anyhow::Result;
use serde_json::{Value, json};

use crate::harness::{AgentContext, Tool};

// Agent tools implemented on top of the harness.
//
// These are the building blocks for higher-level agent tasks such as
// analytics schema refactoring.

pub struct QueryHistoryTool {
    /// Default number of recent queries to return when the caller does not
    /// specify an explicit limit.
    pub default_limit: usize,
}

#[async_trait::async_trait(?Send)]
impl Tool for QueryHistoryTool {
    fn name(&self) -> &'static str {
        "query_history"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(self.default_limit);

        let records = ctx.engine.catalog().recent_queries(limit)?;
        let mut out = Vec::with_capacity(records.len());

        for r in records {
            out.push(json!({
                "id": r.id,
                "submitted_at": r.submitted_at,
                "user": r.user_name,
                "warehouse": r.warehouse,
                "sql": r.sql,
                "duration_ms": r.duration_ms,
                "rows_returned": r.rows_returned,
                "rows_scanned": r.rows_scanned,
                "status": r.status,
            }));
        }

        Ok(json!({ "queries": out }))
    }
}

pub struct SchemaIntrospectTool;

#[async_trait::async_trait(?Send)]
impl Tool for SchemaIntrospectTool {
    fn name(&self) -> &'static str {
        "schema_introspect"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        // Optional table_name filter; when omitted, return all tables in public schema.
        let table_filter = params
            .get("table_name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let sql = if let Some(ref name) = table_filter {
            format!(
                "SELECT table_name, column_name, data_type, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = '{}' \
                 ORDER BY ordinal_position",
                name.replace('\'', "")
            )
        } else {
            "SELECT table_name, column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = 'public' \
             ORDER BY table_name, ordinal_position"
                .to_string()
        };

        let batches = ctx.engine.execute_sql_raw(&sql).await?;
        let mut tables: std::collections::BTreeMap<String, Vec<Value>> =
            std::collections::BTreeMap::new();

        for batch in &batches {
            for i in 0..batch.num_rows() {
                let table_name = arrow::util::display::array_value_to_string(batch.column(0), i)
                    .unwrap_or_default();
                let col_name = arrow::util::display::array_value_to_string(batch.column(1), i)
                    .unwrap_or_default();
                let data_type = arrow::util::display::array_value_to_string(batch.column(2), i)
                    .unwrap_or_default();
                let nullable = arrow::util::display::array_value_to_string(batch.column(3), i)
                    .unwrap_or_default();

                tables.entry(table_name).or_default().push(json!({
                    "name": col_name,
                    "type": data_type,
                    "nullable": nullable == "YES",
                }));
            }
        }

        let mut result = Vec::new();
        for (table, columns) in tables {
            result.push(json!({
                "table": table,
                "columns": columns,
            }));
        }

        Ok(json!({ "tables": result }))
    }
}

/// Migration planner tool.
///
/// For now this is intentionally conservative: it does not apply any changes.
/// It inspects the schema (optionally for a single table) and returns a
/// skeleton plan describing CTAS statements the agent may want to review.
pub struct MigrationPlannerTool;

/// The heuristic fallback action — also the contract the refactor task smoke-tests.
fn heuristic_actions(target: &str) -> Value {
    let name = if target.is_empty() { "fact" } else { target };
    json!([{
        "kind": "ctas_mart",
        "sql": format!("CREATE TABLE {name}_mart AS SELECT * FROM {name};"),
        "rationale": "heuristic: materialize a mart from the source table",
    }])
}

/// Compact "table(col type, …)" summary of the public schema for the LLM.
async fn schema_summary(ctx: &mut AgentContext) -> Result<String> {
    let sql = "SELECT table_name, column_name, data_type FROM information_schema.columns \
               WHERE table_schema = 'public' ORDER BY table_name, ordinal_position";
    let batches = ctx.engine.execute_sql_raw(sql).await?;
    let mut tables: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for batch in &batches {
        for i in 0..batch.num_rows() {
            let t =
                arrow::util::display::array_value_to_string(batch.column(0), i).unwrap_or_default();
            let c =
                arrow::util::display::array_value_to_string(batch.column(1), i).unwrap_or_default();
            let d =
                arrow::util::display::array_value_to_string(batch.column(2), i).unwrap_or_default();
            tables.entry(t).or_default().push(format!("{c} {d}"));
        }
    }
    Ok(tables
        .into_iter()
        .take(60)
        .map(|(t, cols)| format!("{t}({})", cols.join(", ")))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn recent_sql(ctx: &mut AgentContext, limit: usize) -> String {
    ctx.engine
        .catalog()
        .recent_queries(limit)
        .map(|recs| {
            recs.iter()
                .map(|r| r.sql.clone())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[async_trait::async_trait(?Send)]
impl Tool for MigrationPlannerTool {
    fn name(&self) -> &'static str {
        "migration_planner"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let target_table = params
            .get("target_table")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("fact")
            .to_string();

        // No LLM configured → deterministic skeleton (preserves the contract).
        if !crate::llm::available() {
            return Ok(json!({
                "target_table": target_table,
                "planner": "heuristic",
                "note": "Set OPENSNOW_ENABLE_LLM_PLANNER=1 and ANTHROPIC_API_KEY for an LLM-designed refactor plan.",
                "actions": heuristic_actions(&target_table),
            }));
        }

        let schema = schema_summary(ctx).await.unwrap_or_default();
        let history = recent_sql(ctx, 40);
        let system = "You are a senior analytics engineer refactoring a Snowflake-style \
            warehouse into clean staging → core → mart layers. Propose a minimal, \
            dependency-aware refactor that removes redundant staging tables and consolidates \
            duplicated logic. Output STRICT JSON only — no prose outside the JSON.";
        let focus = target_table.clone();
        let user = format!(
            "Warehouse tables (name(columns)):\n{schema}\n\n\
             Recent queries (frequently-referenced tables matter most):\n{history}\n\n\
             Focus: {focus}\n\n\
             Return JSON: {{\"analysis\": \"2-3 sentences on redundancy and the proposed \
             hierarchy\", \"actions\": [{{\"kind\": \"ctas_staging\"|\"ctas_mart\"|\"drop\", \
             \"sql\": \"CREATE TABLE x AS SELECT ...\", \"rationale\": \"why\"}}]}}. \
             Use real table/column names from the schema. Read-only DDL proposals only."
        );

        match crate::llm::complete(system, &user, 2000) {
            Ok(text) => {
                if let Some(v) = crate::llm::extract_json(&text) {
                    let actions = v
                        .get("actions")
                        .filter(|a| a.as_array().map(|x| !x.is_empty()).unwrap_or(false))
                        .cloned()
                        .unwrap_or_else(|| heuristic_actions(&target_table));
                    Ok(json!({
                        "target_table": target_table,
                        "planner": "llm",
                        "model": crate::llm::model(),
                        "analysis": v.get("analysis").cloned().unwrap_or(Value::Null),
                        "actions": actions,
                    }))
                } else {
                    // Couldn't parse JSON — surface raw text, keep a valid action.
                    Ok(json!({
                        "target_table": target_table,
                        "planner": "llm",
                        "model": crate::llm::model(),
                        "plan_text": text,
                        "actions": heuristic_actions(&target_table),
                    }))
                }
            }
            Err(e) => Ok(json!({
                "target_table": target_table,
                "planner": "heuristic",
                "note": format!("LLM call failed ({e}); using heuristic."),
                "actions": heuristic_actions(&target_table),
            })),
        }
    }
}

/// Refactor test tool.
///
/// Given a list of SQL queries, executes them against the current schema and
/// reports basic metrics (row counts, success/error). In future this will be
/// extended to compare pre/post migration schemas.
pub struct RefactorTestTool;

#[async_trait::async_trait(?Send)]
impl Tool for RefactorTestTool {
    fn name(&self) -> &'static str {
        "refactor_test"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let queries: Vec<String> = params
            .get("queries")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let mut tests_out = Vec::with_capacity(queries.len());

        for sql in queries {
            let start = Instant::now();
            let result = ctx.engine.execute_sql(&sql).await;
            let duration_ms = start.elapsed().as_millis() as i64;

            match result {
                Ok(batches) => {
                    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
                    tests_out.push(json!({
                        "sql": sql,
                        "status": "success",
                        "rows": rows,
                        "duration_ms": duration_ms,
                        "error": serde_json::Value::Null,
                    }));
                }
                Err(e) => {
                    tests_out.push(json!({
                        "sql": sql,
                        "status": "error",
                        "rows": 0,
                        "duration_ms": duration_ms,
                        "error": e.to_string(),
                    }));
                }
            }
        }

        Ok(json!({ "tests": tests_out }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensnow_core::{EngineConfig, OpenSnowEngine};

    fn isolated_engine() -> OpenSnowEngine {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_path = dir.path().join("catalog.db");
        let config = EngineConfig {
            warehouse_path: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        };
        // Keep dir alive for the duration of the test by leaking it — it is
        // cleaned up by the OS when the process exits (acceptable in tests).
        std::mem::forget(dir);
        OpenSnowEngine::from_config_and_catalog(config, catalog_path.to_str().unwrap())
    }

    #[tokio::test]
    async fn query_history_tool_returns_empty_list_on_fresh_catalog() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = QueryHistoryTool { default_limit: 10 };
        let out = tool
            .invoke(&mut ctx, json!({}))
            .await
            .expect("tool should not fail");
        assert_eq!(out["queries"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn query_history_tool_respects_explicit_limit() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = QueryHistoryTool { default_limit: 100 };
        let out = tool
            .invoke(&mut ctx, json!({ "limit": 5 }))
            .await
            .expect("tool should not fail");
        assert!(out["queries"].is_array());
    }

    #[tokio::test]
    async fn schema_introspect_tool_returns_empty_tables_on_fresh_engine() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = SchemaIntrospectTool;
        let out = tool
            .invoke(&mut ctx, json!({}))
            .await
            .expect("schema_introspect should not fail on empty engine");
        assert!(
            out["tables"].as_array().unwrap().is_empty(),
            "fresh engine should have no public-schema tables"
        );
    }

    #[tokio::test]
    async fn schema_introspect_tool_accepts_table_filter() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = SchemaIntrospectTool;
        let out = tool
            .invoke(&mut ctx, json!({ "table_name": "nonexistent_table" }))
            .await
            .expect("schema_introspect with filter should not fail");
        assert!(out["tables"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn migration_planner_tool_emits_basic_plan() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = MigrationPlannerTool;
        let out = tool
            .invoke(&mut ctx, json!({ "target_table": "fact_orders" }))
            .await
            .expect("tool should not fail");
        assert_eq!(out["target_table"].as_str(), Some("fact_orders"));
        let actions = out["actions"].as_array().unwrap();
        assert!(!actions.is_empty());
        assert!(
            actions[0]["sql"]
                .as_str()
                .unwrap()
                .contains("CREATE TABLE fact_orders_mart")
        );
    }

    #[tokio::test]
    async fn migration_planner_tool_uses_default_table_when_param_missing() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = MigrationPlannerTool;
        let out = tool
            .invoke(&mut ctx, json!({}))
            .await
            .expect("tool should not fail without target_table");
        assert_eq!(out["target_table"].as_str(), Some("fact"));
        assert!(
            out["actions"][0]["sql"]
                .as_str()
                .unwrap()
                .contains("fact_mart")
        );
    }

    #[tokio::test]
    async fn refactor_test_tool_runs_queries() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = RefactorTestTool;
        let out = tool
            .invoke(&mut ctx, json!({ "queries": ["SELECT 1 AS x"] }))
            .await
            .expect("tool should not fail");
        let tests = out["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["status"].as_str(), Some("success"));
    }

    #[tokio::test]
    async fn refactor_test_tool_handles_multiple_queries() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = RefactorTestTool;
        let out = tool
            .invoke(
                &mut ctx,
                json!({ "queries": ["SELECT 1 AS a", "SELECT 2 AS b", "SELECT 3 AS c"] }),
            )
            .await
            .expect("tool should not fail");
        let tests = out["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 3);
        for t in tests {
            assert_eq!(t["status"].as_str(), Some("success"));
        }
    }

    #[tokio::test]
    async fn refactor_test_tool_reports_error_for_bad_sql() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = RefactorTestTool;
        let out = tool
            .invoke(
                &mut ctx,
                json!({ "queries": ["SELECT * FROM this_table_does_not_exist_xyz"] }),
            )
            .await
            .expect("tool itself should not return Err — error goes in result");
        let tests = out["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0]["status"].as_str(), Some("error"));
        assert!(tests[0]["error"].as_str().is_some());
    }

    #[tokio::test]
    async fn refactor_test_tool_handles_empty_query_list() {
        let engine = isolated_engine();
        let mut ctx = AgentContext::new(engine, "default", None);
        let tool = RefactorTestTool;
        let out = tool
            .invoke(&mut ctx, json!({ "queries": [] }))
            .await
            .expect("tool should not fail");
        assert_eq!(out["tests"].as_array().unwrap().len(), 0);
    }
}
