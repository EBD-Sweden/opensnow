//! Warehouse, table, and materialized-view control-plane tools.
//!
//! These complete the MCP surface so an org's LLM/app can drive the whole
//! platform — not just dbt models and queries. Each tool executes a structured,
//! identifier-validated operation against the engine:
//!
//! - warehouse lifecycle (`warehouse_list`, `warehouse_create`)
//! - external data registration (`register_table` — Parquet by name + URI)
//! - table teardown (`table_drop`)
//! - materialized views (`materialized_view_create` / `_refresh` / `_drop`)
//!
//! Arbitrary SQL (including any DDL these wrap) remains available via the `query`
//! tool; these exist for discoverability, structured inputs, and safe identifier
//! handling.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::harness::{AgentContext, Tool};

/// Unquoted identifiers only: letters, digits, underscore; must start with a
/// letter or underscore. Mirrors the engine's `is_safe_unqualified_identifier`
/// so we never interpolate attacker-controlled text into SQL.
fn safe_ident(name: &str, what: &str) -> Result<String> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if ok {
        Ok(name.to_string())
    } else {
        Err(anyhow!(
            "invalid {what} '{name}': use letters, digits and underscores only (must start with a letter)"
        ))
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow!("missing required parameter '{key}'"))
}

/// Execute SQL and return a compact JSON result (row count + rendered table).
async fn run_sql(ctx: &mut AgentContext, sql: &str, action: &str) -> Result<Value> {
    let batches = ctx.engine.execute_sql(sql).await?;
    let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    let rendered = arrow::util::pretty::pretty_format_batches(&batches)
        .map(|t| t.to_string())
        .unwrap_or_default();
    Ok(json!({
        "status": "ok",
        "action": action,
        "sql": sql,
        "rows": rows,
        "result": rendered,
    }))
}

// ── Warehouses ──────────────────────────────────────────────────────────────

pub struct WarehouseListTool;

#[async_trait::async_trait(?Send)]
impl Tool for WarehouseListTool {
    fn name(&self) -> &'static str {
        "warehouse_list"
    }

    async fn invoke(&self, ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        run_sql(ctx, "SHOW WAREHOUSES", "warehouse_list").await
    }
}

pub struct WarehouseCreateTool;

#[async_trait::async_trait(?Send)]
impl Tool for WarehouseCreateTool {
    fn name(&self) -> &'static str {
        "warehouse_create"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "warehouse name")?;
        let mut clause = String::new();
        if let Some(size) = params.get("size").and_then(|v| v.as_str()) {
            let size = size.to_ascii_lowercase();
            if !matches!(
                size.as_str(),
                "xsmall" | "small" | "medium" | "large" | "xlarge"
            ) {
                return Err(anyhow!(
                    "invalid size '{size}': use xsmall, small, medium, large, or xlarge"
                ));
            }
            clause.push_str(&format!(" SIZE = '{size}'"));
        }
        for (key, sql_key) in [
            ("min_nodes", "MIN_NODES"),
            ("max_nodes", "MAX_NODES"),
            ("auto_suspend_secs", "AUTO_SUSPEND"),
        ] {
            if let Some(n) = params.get(key).and_then(|v| v.as_i64()) {
                clause.push_str(&format!(" {sql_key} = {n}"));
            }
        }
        let sql = if clause.is_empty() {
            format!("CREATE WAREHOUSE {name}")
        } else {
            format!("CREATE WAREHOUSE {name} WITH{clause}")
        };
        run_sql(ctx, &sql, "warehouse_create").await
    }
}

// ── External table registration ─────────────────────────────────────────────

pub struct RegisterTableTool;

#[async_trait::async_trait(?Send)]
impl Tool for RegisterTableTool {
    fn name(&self) -> &'static str {
        "register_table"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "table name")?;
        // Accept `uri` (preferred) or `path` for the Parquet source.
        let uri = required_str(&params, "uri")
            .or_else(|_| required_str(&params, "path"))
            .map_err(|_| anyhow!("missing required parameter 'uri' (Parquet path/URL)"))?;
        ctx.engine.register_parquet(&name, uri).await?;
        Ok(json!({
            "status": "ok",
            "action": "register_table",
            "table": name,
            "uri": uri,
        }))
    }
}

// ── Table teardown ──────────────────────────────────────────────────────────

pub struct TableDropTool;

#[async_trait::async_trait(?Send)]
impl Tool for TableDropTool {
    fn name(&self) -> &'static str {
        "table_drop"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "table name")?;
        let if_exists = params
            .get("if_exists")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let sql = if if_exists {
            format!("DROP TABLE IF EXISTS {name}")
        } else {
            format!("DROP TABLE {name}")
        };
        run_sql(ctx, &sql, "table_drop").await
    }
}

// ── Materialized views ──────────────────────────────────────────────────────

pub struct MaterializedViewCreateTool;

#[async_trait::async_trait(?Send)]
impl Tool for MaterializedViewCreateTool {
    fn name(&self) -> &'static str {
        "materialized_view_create"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "materialized view name")?;
        let select_sql = required_str(&params, "sql")?;
        let sql = format!("CREATE MATERIALIZED VIEW {name} AS {select_sql}");
        run_sql(ctx, &sql, "materialized_view_create").await
    }
}

pub struct MaterializedViewRefreshTool;

#[async_trait::async_trait(?Send)]
impl Tool for MaterializedViewRefreshTool {
    fn name(&self) -> &'static str {
        "materialized_view_refresh"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "materialized view name")?;
        let sql = format!("REFRESH MATERIALIZED VIEW {name}");
        run_sql(ctx, &sql, "materialized_view_refresh").await
    }
}

pub struct MaterializedViewDropTool;

#[async_trait::async_trait(?Send)]
impl Tool for MaterializedViewDropTool {
    fn name(&self) -> &'static str {
        "materialized_view_drop"
    }

    async fn invoke(&self, ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_ident(required_str(&params, "name")?, "materialized view name")?;
        let sql = format!("DROP MATERIALIZED VIEW {name}");
        run_sql(ctx, &sql, "materialized_view_drop").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensnow_core::{EngineConfig, OpenSnowEngine};

    /// Isolated engine (own warehouse dir + catalog) so parallel tests don't
    /// collide on warehouse/table names in a shared default catalog.
    fn ctx() -> AgentContext {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog_path = dir.path().join("catalog.db");
        let config = EngineConfig {
            warehouse_path: dir.path().to_str().unwrap().to_string(),
            ..Default::default()
        };
        std::mem::forget(dir); // keep alive for the test; OS cleans up at exit
        let engine =
            OpenSnowEngine::from_config_and_catalog(config, catalog_path.to_str().unwrap());
        AgentContext::new(engine, "default", None)
    }

    #[test]
    fn safe_ident_rejects_injection() {
        assert!(safe_ident("good_name", "x").is_ok());
        assert!(safe_ident("bad name", "x").is_err());
        assert!(safe_ident("1bad", "x").is_err());
        assert!(safe_ident("drop;--", "x").is_err());
        assert!(safe_ident("", "x").is_err());
    }

    #[tokio::test]
    async fn warehouse_create_rejects_bad_name() {
        let mut c = ctx();
        let err = WarehouseCreateTool
            .invoke(&mut c, json!({ "name": "x; DROP TABLE y" }))
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn warehouse_create_rejects_bad_size() {
        let mut c = ctx();
        let err = WarehouseCreateTool
            .invoke(&mut c, json!({ "name": "wh1", "size": "ginormous" }))
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn warehouse_create_and_list_roundtrip() {
        let mut c = ctx();
        let created = WarehouseCreateTool
            .invoke(
                &mut c,
                json!({ "name": "analytics_wh", "size": "medium", "auto_suspend_secs": 120 }),
            )
            .await
            .expect("create should succeed");
        assert_eq!(created["status"], "ok");
        assert!(created["sql"].as_str().unwrap().contains("SIZE = 'medium'"));
        assert!(created["sql"].as_str().unwrap().contains("AUTO_SUSPEND = 120"));

        let listed = WarehouseListTool
            .invoke(&mut c, json!({}))
            .await
            .expect("list should succeed");
        assert_eq!(listed["status"], "ok");
        assert!(listed["result"].as_str().unwrap().contains("analytics_wh"));
    }

    #[tokio::test]
    async fn materialized_view_create_refresh_drop() {
        let mut c = ctx();
        c.engine
            .execute_sql("CREATE TABLE mv_src AS SELECT 1 AS a")
            .await
            .unwrap();

        let create = MaterializedViewCreateTool
            .invoke(
                &mut c,
                json!({ "name": "mv_one", "sql": "SELECT a FROM mv_src" }),
            )
            .await
            .expect("mv create should succeed");
        assert_eq!(create["status"], "ok");

        let refresh = MaterializedViewRefreshTool
            .invoke(&mut c, json!({ "name": "mv_one" }))
            .await
            .expect("mv refresh should succeed");
        assert_eq!(refresh["status"], "ok");

        let drop = MaterializedViewDropTool
            .invoke(&mut c, json!({ "name": "mv_one" }))
            .await
            .expect("mv drop should succeed");
        assert_eq!(drop["status"], "ok");
    }

    #[tokio::test]
    async fn table_drop_if_exists_is_idempotent() {
        let mut c = ctx();
        let out = TableDropTool
            .invoke(&mut c, json!({ "name": "never_existed" }))
            .await
            .expect("drop if exists should not error");
        assert_eq!(out["status"], "ok");
        assert!(out["sql"].as_str().unwrap().contains("IF EXISTS"));
    }

    #[tokio::test]
    async fn register_table_requires_uri() {
        let mut c = ctx();
        let err = RegisterTableTool
            .invoke(&mut c, json!({ "name": "ext_tbl" }))
            .await;
        assert!(err.is_err());
    }
}
