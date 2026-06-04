use serde_json::{Value, json};

/// Returns tool definitions compatible with OpenAI/Anthropic function calling format.
/// AI agents can discover these tools and use them to interact with OpenSnow.
pub fn get_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "query",
            "description": "Execute a SQL query against the OpenSnow data warehouse. Returns results as JSON. Supports full ANSI SQL including JOINs, aggregations, window functions, CTEs.",
            "parameters": {
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL query to execute"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of rows to return (default: 100)",
                        "default": 100
                    }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": "list_tables",
            "description": "List all tables in the warehouse with their column schemas. Use this first to understand what data is available before writing queries.",
            "parameters": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "describe_table",
            "description": "Get detailed schema information for a specific table, including column names, data types, and sample values.",
            "parameters": {
                "type": "object",
                "properties": {
                    "table_name": {
                        "type": "string",
                        "description": "Name of the table to describe"
                    }
                },
                "required": ["table_name"]
            }
        }),
        json!({
            "name": "create_table",
            "description": "Create a new table from a SQL query (CREATE TABLE AS SELECT). The table is persisted as Parquet in the warehouse.",
            "parameters": {
                "type": "object",
                "properties": {
                    "table_name": {
                        "type": "string",
                        "description": "Name for the new table"
                    },
                    "sql": {
                        "type": "string",
                        "description": "SELECT query whose results become the table data"
                    }
                },
                "required": ["table_name", "sql"]
            }
        }),
        json!({
            "name": "load_data",
            "description": "Load data from a file into a table (COPY INTO). Supports Parquet, CSV, and JSON files from local filesystem or S3.",
            "parameters": {
                "type": "object",
                "properties": {
                    "table_name": {
                        "type": "string",
                        "description": "Target table name"
                    },
                    "source_path": {
                        "type": "string",
                        "description": "Path to source file (local path or s3://bucket/path)"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["parquet", "csv", "json"],
                        "description": "File format (auto-detected from extension if omitted)"
                    }
                },
                "required": ["table_name", "source_path"]
            }
        }),
        json!({
            "name": "suggest_schema",
            "description": "Given a description of the data, suggest an optimal table schema with appropriate data types, partitioning, and clustering.",
            "parameters": {
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Natural language description of the data (e.g., 'customer purchase history with timestamps, product IDs, and amounts')"
                    },
                    "industry": {
                        "type": "string",
                        "enum": ["telecom", "banking", "general"],
                        "description": "Industry context for schema suggestions"
                    },
                    "sample_data": {
                        "type": "string",
                        "description": "Optional sample data (JSON or CSV) to infer types from"
                    }
                },
                "required": ["description"]
            }
        }),
        json!({
            "name": "explain_query",
            "description": "Get the execution plan for a SQL query without running it. Useful for understanding query performance and optimization opportunities.",
            "parameters": {
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "The SQL query to explain"
                    }
                },
                "required": ["sql"]
            }
        }),
        json!({
            "name": "get_catalog",
            "description": "Get full catalog metadata: all databases, schemas, tables, and warehouse capabilities. Use this for a complete overview of the warehouse.",
            "parameters": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
    ]
}
