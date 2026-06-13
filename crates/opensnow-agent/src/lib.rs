// The MCP `tools/list` handler builds one large `serde_json::json!` literal;
// the default macro recursion limit (128) is too low for the full tool set.
#![recursion_limit = "512"]

pub mod agent_tools;
pub mod auto_schema;
pub mod dispatch;
pub mod harness;
pub mod llm;
pub mod mcp;
pub mod metadata_api;
pub mod nl2sql;
pub mod platform_tools;
pub mod schema_refactor_task;
pub mod tools;
pub mod warehouse_tools;

pub use dispatch::{build_runtime, run_task};
