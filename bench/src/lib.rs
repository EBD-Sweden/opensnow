//! OpenSnow microbenchmarks.
//!
//! These are intentionally synthetic — they generate a 1M-row in-memory
//! table on demand and run three representative shapes:
//!
//!   1. `SELECT COUNT(*) FROM bench`
//!   2. `SELECT category, SUM(value) FROM bench GROUP BY category`
//!   3. `SELECT id, value FROM bench WHERE category = 'g_3' AND value > 50.0`
//!
//! Run with `cargo bench -p opensnow-bench`.

use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::datasource::MemTable;
use datafusion::prelude::SessionContext;

/// Number of rows in the synthetic dataset.
pub const ROW_COUNT: usize = 1_000_000;
/// Number of distinct GROUP BY categories.
pub const CATEGORIES: i64 = 8;

/// Build the schema used by every benchmark.
pub fn bench_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("category", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
    ]))
}

/// Produce a deterministic 1M-row `RecordBatch`. Values are cheap to compute
/// so that benchmark warm-up time is dominated by query execution, not data
/// generation.
pub fn generate_batch(rows: usize) -> RecordBatch {
    let schema = bench_schema();
    let ids: Int64Array = (0..rows as i64).collect();
    let categories: StringArray = (0..rows)
        .map(|i| Some(format!("g_{}", (i as i64) % CATEGORIES)))
        .collect();
    let values: Float64Array = (0..rows).map(|i| ((i % 100) as f64) + 0.5).collect();
    RecordBatch::try_new(
        schema,
        vec![Arc::new(ids), Arc::new(categories), Arc::new(values)],
    )
    .expect("schema matches arrays")
}

/// Build a fresh DataFusion `SessionContext` with `bench` registered as an
/// in-memory table. Each benchmark gets its own context so per-session caches
/// (statistics, file lists, etc.) don't leak across measurements.
pub async fn build_context() -> SessionContext {
    let ctx = SessionContext::new();
    let batch = generate_batch(ROW_COUNT);
    let table = MemTable::try_new(bench_schema(), vec![vec![batch]]).expect("memtable");
    ctx.register_table("bench", Arc::new(table))
        .expect("register");
    ctx
}

/// Run a SQL query against a context and collect the result rows. Used by
/// the benchmark targets so the measurement includes the full
/// parse → plan → execute → collect path.
pub async fn run_query(ctx: &SessionContext, sql: &str) -> usize {
    let df = ctx.sql(sql).await.expect("sql");
    let batches = df.collect().await.expect("collect");
    batches.iter().map(|b| b.num_rows()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_has_expected_shape() {
        let batch = generate_batch(1_000);
        assert_eq!(batch.num_rows(), 1_000);
        assert_eq!(batch.num_columns(), 3);
    }

    #[tokio::test]
    async fn count_query_works() {
        let ctx = build_context().await;
        let rows = run_query(&ctx, "SELECT COUNT(*) FROM bench").await;
        assert_eq!(rows, 1);
    }

    #[tokio::test]
    async fn group_by_returns_one_row_per_category() {
        let ctx = build_context().await;
        let rows = run_query(
            &ctx,
            "SELECT category, SUM(value) AS s FROM bench GROUP BY category",
        )
        .await;
        assert_eq!(rows, CATEGORIES as usize);
    }
}
