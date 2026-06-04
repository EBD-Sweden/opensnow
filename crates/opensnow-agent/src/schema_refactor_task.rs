/// AnalyticsSchemaRefactorTask
///
/// Orchestrates a staging → core → mart pipeline refactor using:
///   - SchemaIntrospectTool:  reads current schema
///   - QueryHistoryTool:      finds which tables are actually queried
///   - MigrationPlannerTool:  emits CTAS plan for each heavily-used fact table
///   - RefactorTestTool:      smoke-tests the proposed mart queries
///
/// The task is read-only by design — it proposes a plan and validates it
/// without applying any DDL. A human (or higher-level agent) reviews and
/// executes the DDL from the plan output.
use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use tracing::info;

use crate::harness::{AgentContext, AgentRuntime, AgentTask};

pub struct AnalyticsSchemaRefactorTask {
    /// Tables explicitly specified by the caller; when empty the task picks
    /// candidates from query history (most-queried tables).
    pub target_tables: Vec<String>,

    /// Maximum number of candidate tables to consider when auto-detecting
    /// from query history.
    pub max_tables: usize,

    /// Output of the last run (populated by `run()`).
    pub last_report: Option<Value>,
}

impl Default for AnalyticsSchemaRefactorTask {
    fn default() -> Self {
        Self {
            target_tables: Vec::new(),
            max_tables: 10,
            last_report: None,
        }
    }
}

impl AnalyticsSchemaRefactorTask {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tables(tables: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            target_tables: tables.into_iter().map(|t| t.into()).collect(),
            ..Self::default()
        }
    }
}

#[async_trait::async_trait(?Send)]
impl AgentTask for AnalyticsSchemaRefactorTask {
    fn id(&self) -> &'static str {
        "analytics_schema_refactor"
    }

    async fn run(&mut self, runtime: &AgentRuntime, ctx: &mut AgentContext) -> Result<()> {
        // ── Step 1: introspect schema ─────────────────────────────────────────
        info!("Step 1: introspecting schema");
        let schema_result = runtime
            .invoke_tool("schema_introspect", ctx, json!({}))
            .await
            .context("schema_introspect failed")?;

        let tables_in_schema: Vec<String> = schema_result["tables"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|t| t["table"].as_str().map(|s| s.to_string()))
            .collect();

        info!("Schema has {} tables", tables_in_schema.len());

        // ── Step 2: get query history to find hot tables ──────────────────────
        info!("Step 2: reading query history");
        let history_result = runtime
            .invoke_tool("query_history", ctx, json!({ "limit": 200 }))
            .await
            .context("query_history failed")?;

        // Count how many times each table name appears in recent SQL.
        let mut mention_count: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        if let Some(queries) = history_result["queries"].as_array() {
            for q in queries {
                if let Some(sql) = q["sql"].as_str() {
                    let sql_lower = sql.to_lowercase();
                    for table in &tables_in_schema {
                        if sql_lower.contains(&table.to_lowercase()) {
                            *mention_count.entry(table.clone()).or_default() += 1;
                        }
                    }
                }
            }
        }

        // ── Step 3: determine candidate tables ───────────────────────────────
        let candidates: Vec<String> = if !self.target_tables.is_empty() {
            self.target_tables.clone()
        } else {
            // Auto-pick: most frequently mentioned fact/staging tables.
            let mut sorted: Vec<_> = mention_count.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));
            sorted
                .into_iter()
                .take(self.max_tables)
                .map(|(t, _)| t.clone())
                .collect()
        };

        info!("Refactor candidates: {:?}", candidates);

        // ── Step 4: run migration planner per candidate ───────────────────────
        info!("Step 4: generating migration plans");
        let mut plans: Vec<Value> = Vec::new();

        for table in &candidates {
            let plan = runtime
                .invoke_tool("migration_planner", ctx, json!({ "target_table": table }))
                .await
                .context("migration_planner failed")?;

            plans.push(plan);
        }

        // ── Step 5: build smoke-test queries from the plans ───────────────────
        info!("Step 5: smoke-testing proposed marts");
        let mut smoke_queries: Vec<String> = Vec::new();

        for plan in &plans {
            if let Some(actions) = plan["actions"].as_array() {
                for action in actions {
                    if action["kind"].as_str() == Some("ctas_mart")
                        && let Some(sql) = action["sql"].as_str()
                    {
                        // Convert CREATE TABLE … AS SELECT … → SELECT … (dry run)
                        let select_sql = sql
                            .split_once("AS SELECT")
                            .map(|(_, s)| format!("SELECT{}", s.trim_end_matches(';')))
                            .unwrap_or_else(|| "SELECT 1".to_string());
                        smoke_queries.push(select_sql);
                    }
                }
            }
        }

        let test_results = if !smoke_queries.is_empty() {
            runtime
                .invoke_tool("refactor_test", ctx, json!({ "queries": smoke_queries }))
                .await
                .context("refactor_test failed")?
        } else {
            json!({ "tests": [] })
        };

        // ── Step 6: assemble final report ────────────────────────────────────
        let passed: usize = test_results["tests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|t| t["status"].as_str() == Some("success"))
            .count();

        let total = smoke_queries.len();

        info!(
            "Refactor task complete: {} candidates, {}/{} smoke tests passed",
            candidates.len(),
            passed,
            total
        );

        self.last_report = Some(json!({
            "candidates": candidates,
            "plans": plans,
            "smoke_tests": test_results["tests"],
            "summary": {
                "candidates_count": candidates.len(),
                "tests_total": total,
                "tests_passed": passed,
                "tests_failed": total - passed,
            }
        }));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensnow_core::{EngineConfig, OpenSnowEngine};

    use crate::agent_tools::{
        MigrationPlannerTool, QueryHistoryTool, RefactorTestTool, SchemaIntrospectTool,
    };
    use crate::harness::AgentRuntime;

    fn make_runtime() -> AgentRuntime {
        let mut rt = AgentRuntime::new();
        rt.register_tool(SchemaIntrospectTool);
        rt.register_tool(QueryHistoryTool { default_limit: 50 });
        rt.register_tool(MigrationPlannerTool);
        rt.register_tool(RefactorTestTool);
        rt
    }

    #[tokio::test]
    async fn refactor_task_runs_end_to_end_with_explicit_tables() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let mut ctx = AgentContext::new(engine, "default", None);
        let runtime = make_runtime();

        let mut task = AnalyticsSchemaRefactorTask::with_tables(["fact_orders"]);
        task.run(&runtime, &mut ctx)
            .await
            .expect("task should complete without error");

        let report = task.last_report.as_ref().expect("report should be set");

        // At least one candidate was processed.
        assert_eq!(report["summary"]["candidates_count"].as_u64(), Some(1));
        // Plans were generated.
        let plans = report["plans"].as_array().unwrap();
        assert!(!plans.is_empty());
        // The first plan targets fact_orders.
        assert_eq!(plans[0]["target_table"].as_str(), Some("fact_orders"));
    }

    #[tokio::test]
    async fn refactor_task_runs_with_no_explicit_tables() {
        // Empty schema → no candidates → graceful no-op.
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let mut ctx = AgentContext::new(engine, "default", None);
        let runtime = make_runtime();

        let mut task = AnalyticsSchemaRefactorTask::new();
        task.run(&runtime, &mut ctx)
            .await
            .expect("task should complete without error");

        let report = task.last_report.as_ref().unwrap();
        assert_eq!(report["summary"]["candidates_count"].as_u64(), Some(0));
    }
}
