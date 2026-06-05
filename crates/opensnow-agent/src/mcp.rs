use std::sync::Arc;

use opensnow_core::OpenSnowEngine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::info;

use crate::dispatch::build_runtime;
use crate::harness::AgentContext;

/// MCP (Model Context Protocol) server implementation.
/// Allows AI agents (Claude, GPT, etc.) to interact with OpenSnow as a tool provider.
///
/// MCP Protocol: JSON-RPC 2.0 over stdio or HTTP.
/// See: https://modelcontextprotocol.io

#[derive(Debug, Serialize, Deserialize)]
pub struct McpRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct McpResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<McpError>,
}

#[derive(Debug, Serialize)]
pub struct McpError {
    pub code: i32,
    pub message: String,
}

pub struct McpServer {
    engine: Arc<OpenSnowEngine>,
}

impl McpServer {
    pub fn new(engine: Arc<OpenSnowEngine>) -> Self {
        Self { engine }
    }

    /// Handle a single MCP JSON-RPC request.
    pub async fn handle_request(&self, request: McpRequest) -> McpResponse {
        let id = request.id.clone();

        let result = match request.method.as_str() {
            "initialize" => self.handle_initialize().await,
            "tools/list" => self.handle_tools_list().await,
            "tools/call" => self.handle_tool_call(request.params).await,
            "resources/list" => self.handle_resources_list().await,
            "resources/read" => self.handle_resource_read(request.params).await,
            _ => Err(McpError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
            }),
        };

        match result {
            Ok(value) => McpResponse {
                jsonrpc: "2.0".into(),
                id,
                result: Some(value),
                error: None,
            },
            Err(error) => McpResponse {
                jsonrpc: "2.0".into(),
                id,
                result: None,
                error: Some(error),
            },
        }
    }

    async fn handle_initialize(&self) -> Result<Value, McpError> {
        Ok(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {},
                "resources": {}
            },
            "serverInfo": {
                "name": "opensnow",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "OpenSnow Analytics Data Warehouse — agent-native SQL engine for telecom and banking analytics"
            }
        }))
    }

    async fn handle_tools_list(&self) -> Result<Value, McpError> {
        Ok(json!({
            "tools": [
                {
                    "name": "query",
                    "description": "Execute a SQL query against the OpenSnow warehouse. Returns results as a formatted table. Supports full ANSI SQL including JOINs, aggregations, window functions, CTEs, and UNION.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "sql": { "type": "string", "description": "SQL query to execute" }
                        },
                        "required": ["sql"]
                    }
                },
                {
                    "name": "list_tables",
                    "description": "List all tables in the warehouse with column count and descriptions.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
                    }
                },
                {
                    "name": "describe_table",
                    "description": "Get the full schema of a table: column names, types, nullability, and sample values.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "table_name": { "type": "string", "description": "Table name to describe" }
                        },
                        "required": ["table_name"]
                    }
                },
                {
                    "name": "create_table",
                    "description": "Create a new persistent table from a SQL SELECT query.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "table_name": { "type": "string", "description": "Name for the new table" },
                            "select_sql": { "type": "string", "description": "SELECT query whose results become the table" }
                        },
                        "required": ["table_name", "select_sql"]
                    }
                },
                {
                    "name": "suggest_schema",
                    "description": "Suggest a table schema based on a natural language description of the data. Returns column definitions, partitioning strategy, and CREATE TABLE SQL.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "description": { "type": "string", "description": "Describe the data you want to store" },
                            "industry": { "type": "string", "enum": ["telecom", "banking", "general"], "description": "Industry context" }
                        },
                        "required": ["description"]
                    }
                },
                {
                    "name": "schema_introspect",
                    "description": "Introspect the full warehouse schema. Returns all tables in the public schema with their columns, types, and nullability. Optionally filter to a single table.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "table_name": { "type": "string", "description": "Optional table name filter" }
                        }
                    }
                },
                {
                    "name": "query_history",
                    "description": "Retrieve recent query history from the catalog. Useful for identifying hot tables and usage patterns.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "limit": { "type": "integer", "description": "Max queries to return (default 100)" }
                        }
                    }
                },
                {
                    "name": "migration_planner",
                    "description": "Generate a CTAS migration plan for a target table (staging → mart). Returns proposed SQL without executing it.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "target_table": { "type": "string", "description": "Source table to build a mart from" }
                        },
                        "required": ["target_table"]
                    }
                },
                {
                    "name": "refactor_test",
                    "description": "Smoke-test a list of SQL queries against the current schema. Returns per-query status, row counts, and error messages.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "queries": { "type": "array", "items": { "type": "string" }, "description": "SQL queries to test" }
                        },
                        "required": ["queries"]
                    }
                },
                {
                    "name": "dbt_list_models",
                    "description": "List every dbt model (pipeline step) in the project with its layer (staging/mart).",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "dbt_get_model",
                    "description": "Return the SQL source of one dbt model by name.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "name": { "type": "string", "description": "Model name (no .sql)" } },
                        "required": ["name"]
                    }
                },
                {
                    "name": "dbt_write_model",
                    "description": "Create or overwrite a dbt model's SQL. Use ref()/source() in the SQL. Run pipeline_run afterwards to build it.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "description": "Model name (letters/digits/underscore)" },
                            "sql": { "type": "string", "description": "The model SQL" },
                            "layer": { "type": "string", "enum": ["staging", "marts"], "description": "Subfolder for new models (default marts)" }
                        },
                        "required": ["name", "sql"]
                    }
                },
                {
                    "name": "dbt_delete_model",
                    "description": "Delete a dbt model file by name.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "name": { "type": "string" } },
                        "required": ["name"]
                    }
                },
                {
                    "name": "pipeline_run",
                    "description": "Run the pipeline (dbt run) in dependency order, building models on OpenSnow. Returns success and a log tail.",
                    "inputSchema": {
                        "type": "object",
                        "properties": { "select": { "type": "string", "description": "Optional dbt --select expression" } }
                    }
                },
                {
                    "name": "pipeline_status",
                    "description": "Read the pipeline DAG and last-run status from dbt artifacts (models, dependencies, per-node status).",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "schedule_get",
                    "description": "Read the configured pipeline schedule (cron/interval).",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "schedule_set",
                    "description": "Set the pipeline schedule. Provide 'cron' (e.g. '0 6 * * *') or 'interval_secs'.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "cron": { "type": "string", "description": "5/6-field cron expression" },
                            "interval_secs": { "type": "integer", "description": "Fixed interval in seconds" }
                        }
                    }
                }
            ]
        }))
    }

    async fn handle_tool_call(&self, params: Option<Value>) -> Result<Value, McpError> {
        let params = params.ok_or(McpError {
            code: -32602,
            message: "Missing params".into(),
        })?;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or(McpError {
                code: -32602,
                message: "Missing tool name".into(),
            })?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));

        info!("MCP tool call: {} with {:?}", name, args);

        match name {
            "query" => {
                let sql = args.get("sql").and_then(|v| v.as_str()).ok_or(McpError {
                    code: -32602,
                    message: "Missing sql parameter".into(),
                })?;

                match self.engine.execute_sql(sql).await {
                    Ok(batches) => {
                        let formatted = arrow::util::pretty::pretty_format_batches(&batches)
                            .map(|t| t.to_string())
                            .unwrap_or_else(|_| "No results".to_string());
                        let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
                        Ok(json!({
                            "content": [{
                                "type": "text",
                                "text": format!("{}\n({} rows)", formatted, rows)
                            }]
                        }))
                    }
                    Err(e) => Ok(json!({
                        "content": [{"type": "text", "text": format!("Error: {}", e)}],
                        "isError": true
                    })),
                }
            }

            "list_tables" => match self.engine.execute_sql("SHOW TABLES").await {
                Ok(batches) => {
                    let formatted = arrow::util::pretty::pretty_format_batches(&batches)
                        .map(|t| t.to_string())
                        .unwrap_or_default();
                    Ok(json!({ "content": [{"type": "text", "text": formatted}] }))
                }
                Err(e) => Ok(
                    json!({ "content": [{"type": "text", "text": e.to_string()}], "isError": true }),
                ),
            },

            "describe_table" => {
                let table = args
                    .get("table_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !is_safe_identifier(table) {
                    return Err(McpError {
                        code: -32602,
                        message: "Invalid table_name".into(),
                    });
                }
                match self.engine.execute_sql(&format!("DESCRIBE {table}")).await {
                    Ok(batches) => {
                        let formatted = arrow::util::pretty::pretty_format_batches(&batches)
                            .map(|t| t.to_string())
                            .unwrap_or_default();
                        Ok(json!({ "content": [{"type": "text", "text": formatted}] }))
                    }
                    Err(e) => Ok(
                        json!({ "content": [{"type": "text", "text": e.to_string()}], "isError": true }),
                    ),
                }
            }

            "create_table" => {
                let table = args
                    .get("table_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("new_table");
                if !is_safe_identifier(table) {
                    return Err(McpError {
                        code: -32602,
                        message: "Invalid table_name".into(),
                    });
                }
                let sql = args
                    .get("select_sql")
                    .and_then(|v| v.as_str())
                    .ok_or(McpError {
                        code: -32602,
                        message: "Missing select_sql".into(),
                    })?;
                let create_sql = format!("CREATE TABLE {table} AS {sql}");
                match self.engine.execute_sql(&create_sql).await {
                    Ok(batches) => {
                        let formatted = arrow::util::pretty::pretty_format_batches(&batches)
                            .map(|t| t.to_string())
                            .unwrap_or_default();
                        Ok(json!({ "content": [{"type": "text", "text": formatted}] }))
                    }
                    Err(e) => Ok(
                        json!({ "content": [{"type": "text", "text": e.to_string()}], "isError": true }),
                    ),
                }
            }

            "suggest_schema" => {
                let desc = args
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let industry = args.get("industry").and_then(|v| v.as_str());
                let suggestion = crate::auto_schema::suggest_schema(desc, industry);
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": format!(
                            "Suggested schema for '{}':\n\n{}\n\nPartition by: {:?}\nCluster by: {:?}\n\nRationale: {}",
                            desc, suggestion.create_sql, suggestion.partition_by, suggestion.cluster_by, suggestion.rationale
                        )
                    }]
                }))
            }

            // ── Analytics + platform tools routed through AgentRuntime ─────
            "schema_introspect" | "query_history" | "migration_planner" | "refactor_test"
            | "dbt_list_models" | "dbt_get_model" | "dbt_write_model" | "dbt_delete_model"
            | "pipeline_run" | "pipeline_status" | "schedule_get" | "schedule_set" => {
                let mut ctx = AgentContext::new(Arc::clone(&self.engine), "default", None);
                let runtime = build_runtime();

                match runtime.invoke_tool(name, &mut ctx, args).await {
                    Ok(result) => {
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        Ok(json!({ "content": [{"type": "text", "text": text}] }))
                    }
                    Err(e) => Ok(json!({
                        "content": [{"type": "text", "text": format!("Error: {e}")}],
                        "isError": true
                    })),
                }
            }

            _ => Err(McpError {
                code: -32601,
                message: format!("Unknown tool: {name}"),
            }),
        }
    }

    async fn handle_resources_list(&self) -> Result<Value, McpError> {
        let mut resources = Vec::new();

        if let Ok(batches) = self
            .engine
            .execute_sql_raw(
                "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public'",
            )
            .await
        {
            for batch in &batches {
                for i in 0..batch.num_rows() {
                    let name = arrow::util::display::array_value_to_string(batch.column(0), i)
                        .unwrap_or_default();
                    resources.push(json!({
                        "uri": format!("opensnow://tables/{}", name),
                        "name": name,
                        "description": format!("Table: {}", name),
                        "mimeType": "application/json"
                    }));
                }
            }
        }

        Ok(json!({ "resources": resources }))
    }

    async fn handle_resource_read(&self, params: Option<Value>) -> Result<Value, McpError> {
        let params = params.ok_or(McpError {
            code: -32602,
            message: "Missing params".into(),
        })?;
        let uri = params.get("uri").and_then(|v| v.as_str()).ok_or(McpError {
            code: -32602,
            message: "Missing uri".into(),
        })?;

        // Parse opensnow://tables/{name}
        let table_name = uri.strip_prefix("opensnow://tables/").ok_or(McpError {
            code: -32602,
            message: format!("Invalid resource URI: {uri}"),
        })?;
        if !is_safe_identifier(table_name) {
            return Err(McpError {
                code: -32602,
                message: "Invalid resource table name".into(),
            });
        }

        // Return schema + sample data
        let schema_sql = format!(
            "SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name = '{}' ORDER BY ordinal_position",
            table_name
        );
        let sample_sql = format!("SELECT * FROM {} LIMIT 5", table_name);

        let schema_text = match self.engine.execute_sql_raw(&schema_sql).await {
            Ok(b) => arrow::util::pretty::pretty_format_batches(&b)
                .map(|t| t.to_string())
                .unwrap_or_default(),
            Err(e) => format!("Error: {e}"),
        };

        let sample_text = match self.engine.execute_sql_raw(&sample_sql).await {
            Ok(b) => arrow::util::pretty::pretty_format_batches(&b)
                .map(|t| t.to_string())
                .unwrap_or_default(),
            Err(e) => format!("Error: {e}"),
        };

        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "text/plain",
                "text": format!("Schema for {}:\n{}\n\nSample data:\n{}", table_name, schema_text, sample_text)
            }]
        }))
    }
}

fn is_safe_identifier(identifier: &str) -> bool {
    !identifier.is_empty()
        && identifier
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && identifier
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}
