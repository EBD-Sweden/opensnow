pub mod functions;
pub mod rewriter;

use datafusion::prelude::SessionContext;

/// Register all Snowflake-compatible functions and rewrites with a DataFusion session.
pub fn register_snowflake_compat(ctx: &SessionContext) {
    functions::register_snowflake_functions(ctx);
    tracing::info!("Snowflake SQL compatibility layer registered");
}
