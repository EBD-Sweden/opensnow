use anyhow::Result;
use opensnow_core::OpenSnowEngine;
use serde_json::Value;

use crate::agent_tools::{
    MigrationPlannerTool, QueryHistoryTool, RefactorTestTool, SchemaIntrospectTool,
};
use crate::harness::{AgentContext, AgentRuntime, AgentTask};
use crate::platform_tools::{
    DashboardCreateTool, DashboardListTool, DbtDeleteModelTool, DbtGetModelTool, DbtListModelsTool,
    DbtWriteModelTool, PipelineRunTool, PipelineStatusTool, ScheduleGetTool, ScheduleSetTool,
};
use crate::schema_refactor_task::AnalyticsSchemaRefactorTask;

/// Build a fully-wired `AgentRuntime` with all analytics tools registered.
///
/// The returned runtime can be handed to any `AgentTask` or used directly
/// via `invoke_tool` for one-shot calls.
pub fn build_runtime() -> AgentRuntime {
    let mut rt = AgentRuntime::new();
    rt.register_tool(SchemaIntrospectTool);
    rt.register_tool(QueryHistoryTool { default_limit: 100 });
    rt.register_tool(MigrationPlannerTool);
    rt.register_tool(RefactorTestTool);
    // Platform control-plane: manage dbt models, run the pipeline, set schedule.
    rt.register_tool(DbtListModelsTool);
    rt.register_tool(DbtGetModelTool);
    rt.register_tool(DbtWriteModelTool);
    rt.register_tool(DbtDeleteModelTool);
    rt.register_tool(PipelineRunTool);
    rt.register_tool(PipelineStatusTool);
    rt.register_tool(ScheduleGetTool);
    rt.register_tool(ScheduleSetTool);
    rt.register_tool(DashboardListTool);
    rt.register_tool(DashboardCreateTool);
    rt
}

/// Run a named agent task against the given engine and return its report as JSON.
///
/// Supported tasks:
/// - `"analytics_schema_refactor"` — params: `{ "tables": ["t1", "t2"] }` (optional)
pub async fn run_task(task_name: &str, engine: OpenSnowEngine, params: Value) -> Result<Value> {
    let runtime = build_runtime();
    let mut ctx = AgentContext::new(engine, "default", None);

    match task_name {
        "analytics_schema_refactor" => {
            let tables: Vec<String> = params
                .get("tables")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let mut task = if tables.is_empty() {
                AnalyticsSchemaRefactorTask::new()
            } else {
                AnalyticsSchemaRefactorTask::with_tables(tables)
            };

            task.run(&runtime, &mut ctx).await?;
            Ok(task
                .last_report
                .unwrap_or(serde_json::json!({ "status": "completed" })))
        }
        other => anyhow::bail!("unknown task: {}", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensnow_core::{EngineConfig, OpenSnowEngine};

    #[test]
    fn build_runtime_registers_all_tools() {
        let rt = build_runtime();
        assert!(rt.has_tool("schema_introspect"));
        assert!(rt.has_tool("query_history"));
        assert!(rt.has_tool("migration_planner"));
        assert!(rt.has_tool("refactor_test"));
    }

    #[tokio::test]
    async fn run_task_analytics_schema_refactor_with_explicit_tables() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let report = run_task(
            "analytics_schema_refactor",
            engine,
            serde_json::json!({ "tables": ["fact_orders"] }),
        )
        .await
        .expect("run_task should succeed");

        assert_eq!(report["summary"]["candidates_count"].as_u64(), Some(1));
    }

    #[tokio::test]
    async fn run_task_analytics_schema_refactor_empty_params() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let report = run_task("analytics_schema_refactor", engine, serde_json::json!({}))
            .await
            .expect("run_task should succeed on empty warehouse");

        assert!(report["summary"].is_object());
    }

    #[tokio::test]
    async fn run_task_unknown_returns_error() {
        let engine = OpenSnowEngine::with_config(EngineConfig::default());
        let result = run_task("nonexistent_task", engine, serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
