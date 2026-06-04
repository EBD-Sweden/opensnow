use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use opensnow_core::EngineHandle;
use serde::{Deserialize, Serialize};

/// Agent-friendly metadata API — wired into the MCP HTTP server under `/agent/v1`.
pub fn create_agent_router() -> Router<EngineHandle> {
    Router::new()
        .route("/catalog", get(get_catalog))
        .route("/tables", get(list_tables))
        .route("/tables/{name}", get(get_table_schema))
        .route("/tables/{name}/sample", get(get_sample_data))
        .route("/suggest", get(suggest_query))
        .route("/explain", post(explain_query))
        .route("/tools", get(list_tools))
}

#[derive(Serialize)]
struct CatalogInfo {
    database: String,
    schemas: Vec<SchemaInfo>,
    total_tables: usize,
    total_rows_estimate: u64,
    engine: String,
    version: String,
    capabilities: Vec<String>,
}

#[derive(Serialize)]
struct SchemaInfo {
    name: String,
    tables: Vec<TableSummary>,
}

#[derive(Serialize, Clone)]
struct TableSummary {
    name: String,
    columns: Vec<ColumnInfo>,
    row_count_estimate: u64,
    size_bytes_estimate: u64,
    format: String,
    description: Option<String>,
}

#[derive(Serialize, Clone)]
struct ColumnInfo {
    name: String,
    data_type: String,
    nullable: bool,
    description: Option<String>,
    sample_values: Vec<String>,
    stats: Option<ColumnStats>,
}

#[derive(Serialize, Clone)]
struct ColumnStats {
    min: Option<String>,
    max: Option<String>,
    null_count: u64,
    distinct_count_estimate: u64,
}

async fn get_catalog(State(handle): State<EngineHandle>) -> Json<CatalogInfo> {
    let tables = get_table_list(&handle).await;
    let total_tables = tables.len();

    Json(CatalogInfo {
        database: "opensnow".to_string(),
        schemas: vec![SchemaInfo {
            name: "public".to_string(),
            tables,
        }],
        total_tables,
        total_rows_estimate: 0,
        engine: "Apache DataFusion".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: vec![
            "sql".to_string(),
            "parquet".to_string(),
            "csv".to_string(),
            "json".to_string(),
            "create_table_as".to_string(),
            "copy_into".to_string(),
            "show_tables".to_string(),
            "describe".to_string(),
            "time_travel".to_string(),
            "s3_object_store".to_string(),
        ],
    })
}

async fn list_tables(State(handle): State<EngineHandle>) -> Json<Vec<TableSummary>> {
    Json(get_table_list(&handle).await)
}

async fn get_table_schema(
    State(handle): State<EngineHandle>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    let sql = format!(
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name = '{}' ORDER BY ordinal_position",
        name.replace('\'', "")
    );
    match handle.execute_sql(&sql).await {
        Ok(batches) => {
            let mut columns = Vec::new();
            for batch in &batches {
                for i in 0..batch.num_rows() {
                    let col_name = arrow::util::display::array_value_to_string(batch.column(0), i)
                        .unwrap_or_default();
                    let data_type = arrow::util::display::array_value_to_string(batch.column(1), i)
                        .unwrap_or_default();
                    let nullable = arrow::util::display::array_value_to_string(batch.column(2), i)
                        .unwrap_or_default();
                    columns.push(serde_json::json!({
                        "name": col_name,
                        "type": data_type,
                        "nullable": nullable == "YES"
                    }));
                }
            }

            let sample = match handle
                .execute_sql(&format!("SELECT * FROM {} LIMIT 3", name))
                .await
            {
                Ok(b) if !b.is_empty() => {
                    let buf = Vec::new();
                    let mut writer = arrow::json::LineDelimitedWriter::new(buf);
                    writer.write(&b[0]).ok();
                    writer.finish().ok();
                    String::from_utf8(writer.into_inner()).unwrap_or_default()
                }
                _ => String::new(),
            };

            Json(serde_json::json!({
                "table": name,
                "columns": columns,
                "sample_rows": sample,
                "sql_hints": {
                    "select_all": format!("SELECT * FROM {} LIMIT 10", name),
                    "count": format!("SELECT COUNT(*) FROM {}", name),
                    "describe": format!("DESCRIBE {}", name),
                }
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn get_sample_data(
    State(handle): State<EngineHandle>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Json<serde_json::Value> {
    match handle
        .execute_sql(&format!("SELECT * FROM {} LIMIT 5", name))
        .await
    {
        Ok(batches) if !batches.is_empty() => {
            let buf = Vec::new();
            let mut writer = arrow::json::LineDelimitedWriter::new(buf);
            writer.write(&batches[0]).ok();
            writer.finish().ok();
            let json_str = String::from_utf8(writer.into_inner()).unwrap_or_default();
            Json(serde_json::json!({
                "table": name,
                "rows": json_str,
                "row_count": batches[0].num_rows()
            }))
        }
        Ok(_) => Json(serde_json::json!({"table": name, "rows": [], "row_count": 0})),
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

#[derive(Deserialize)]
struct SuggestParams {
    intent: Option<String>,
}

async fn suggest_query(
    State(handle): State<EngineHandle>,
    axum::extract::Query(params): axum::extract::Query<SuggestParams>,
) -> Json<serde_json::Value> {
    let tables = get_table_list(&handle).await;
    let table_names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    let suggestions = generate_query_suggestions(&table_names, params.intent.as_deref());

    Json(serde_json::json!({
        "available_tables": table_names,
        "suggested_queries": suggestions,
        "hint": "Use /agent/v1/tables/{name} to see column details before writing queries"
    }))
}

#[derive(Deserialize)]
struct ExplainRequest {
    sql: String,
}

async fn explain_query(
    State(handle): State<EngineHandle>,
    Json(req): Json<ExplainRequest>,
) -> Json<serde_json::Value> {
    let explain_sql = format!("EXPLAIN {}", req.sql);
    match handle.execute_sql(&explain_sql).await {
        Ok(batches) => {
            let mut plan_lines = Vec::new();
            for batch in &batches {
                for i in 0..batch.num_rows() {
                    let line = arrow::util::display::array_value_to_string(batch.column(0), i)
                        .unwrap_or_default();
                    plan_lines.push(line);
                }
            }
            Json(serde_json::json!({
                "sql": req.sql,
                "plan": plan_lines,
                "optimization_hints": analyze_plan(&plan_lines),
            }))
        }
        Err(e) => Json(serde_json::json!({"error": e.to_string()})),
    }
}

async fn list_tools(State(_handle): State<EngineHandle>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "tools": crate::tools::get_tool_definitions(),
    }))
}

fn generate_query_suggestions(tables: &[&str], intent: Option<&str>) -> Vec<serde_json::Value> {
    let mut suggestions = Vec::new();

    if tables.contains(&"cdrs") {
        suggestions.push(serde_json::json!({
            "description": "CDR call volume by type",
            "sql": "SELECT call_type, COUNT(*) AS calls, ROUND(AVG(duration_seconds),1) AS avg_duration FROM cdrs GROUP BY call_type ORDER BY calls DESC",
            "industry": "telecom"
        }));
    }
    if tables.contains(&"transactions") {
        suggestions.push(serde_json::json!({
            "description": "Transaction volume by type and channel",
            "sql": "SELECT txn_type, channel, COUNT(*) AS cnt, ROUND(SUM(amount),2) AS total FROM transactions GROUP BY txn_type, channel ORDER BY total DESC",
            "industry": "banking"
        }));
    }
    if tables.contains(&"cdrs") && tables.contains(&"towers") {
        suggestions.push(serde_json::json!({
            "description": "Call volume by tower region",
            "sql": "SELECT t.region, COUNT(*) AS calls FROM cdrs c JOIN towers t ON c.tower_id = t.tower_id GROUP BY t.region ORDER BY calls DESC",
            "industry": "telecom"
        }));
    }
    if tables.contains(&"transactions") && tables.contains(&"customers") {
        suggestions.push(serde_json::json!({
            "description": "Transaction volume by customer segment",
            "sql": "SELECT c.segment, COUNT(*) AS txns, ROUND(SUM(t.amount),2) AS total FROM transactions t JOIN accounts a ON t.account_from = a.iban JOIN customers c ON a.customer_id = c.customer_id GROUP BY c.segment",
            "industry": "banking"
        }));
    }

    if let Some(intent) = intent {
        suggestions.push(serde_json::json!({
            "description": format!("Custom query for intent: {}", intent),
            "hint": "Use /agent/v1/tables/{name} to explore columns, then write your SQL",
            "available_tables": tables,
        }));
    }

    suggestions
}

fn analyze_plan(plan_lines: &[String]) -> Vec<String> {
    let mut hints = Vec::new();
    let plan_text = plan_lines.join("\n");

    if plan_text.contains("FilterExec") && plan_text.contains("ParquetExec") {
        hints.push("Filter is pushed down to Parquet scan — good for performance".to_string());
    }
    if plan_text.contains("SortExec") && plan_text.contains("GlobalLimitExec") {
        hints.push("Consider adding ORDER BY to your query for deterministic results".to_string());
    }
    if plan_text.contains("HashJoinExec") {
        hints
            .push("Hash join detected — ensure the smaller table is on the build side".to_string());
    }
    if plan_text.contains("CoalesceBatchesExec") {
        hints
            .push("Batch coalescing active — query processes data in efficient chunks".to_string());
    }
    if hints.is_empty() {
        hints.push("Query plan looks efficient".to_string());
    }
    hints
}

async fn get_table_list(handle: &EngineHandle) -> Vec<TableSummary> {
    let mut tables = Vec::new();
    if let Ok(batches) = handle.execute_sql(
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name"
    ).await {
        for batch in &batches {
            for i in 0..batch.num_rows() {
                let name = arrow::util::display::array_value_to_string(batch.column(0), i).unwrap_or_default();
                tables.push(TableSummary {
                    name,
                    columns: vec![],
                    row_count_estimate: 0,
                    size_bytes_estimate: 0,
                    format: "parquet".to_string(),
                    description: None,
                });
            }
        }
    }
    tables
}
