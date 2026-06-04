//! Scatter-gather partitioning and result merging.
//!
//! These are pure functions (no I/O) — the orchestration that actually
//! dispatches fragments to workers lives in [`crate::distributed_executor`].
//! Splitting them out keeps the protocol logic easy to unit-test.

use anyhow::{Result, bail};
use arrow::array::RecordBatch;
use serde::{Deserialize, Serialize};

/// How a query should be split across N partitions.
#[derive(Debug, Clone)]
pub enum PartitionStrategy {
    /// Hash a column modulo `num_partitions`. Each row lands in exactly one
    /// partition — safe for SELECT-style queries when no global aggregation
    /// happens above the partitioned scan.
    HashColumn(String),
    /// Replicate the query to every worker unchanged. The coordinator merges
    /// by concatenation — useful when each worker owns a disjoint shard
    /// of the data and runs the same SQL locally.
    Replicate,
}

impl PartitionStrategy {
    /// Default strategy when the caller does not specify one: replicate.
    pub fn default_strategy() -> Self {
        PartitionStrategy::Replicate
    }
}

/// A planned per-partition query fragment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionedFragment {
    pub partition_id: u32,
    pub total_partitions: u32,
    /// SQL to run on the assigned worker (already rewritten per
    /// [`PartitionStrategy`]).
    pub sql: String,
}

/// A scatter-gather plan: one fragment per partition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionedPlan {
    pub query_id: String,
    pub fragments: Vec<PartitionedFragment>,
}

/// Split `sql` into `num_partitions` fragments.
///
/// `num_partitions` is clamped to at least 1; passing 0 yields a single
/// pass-through fragment (no rewrite). The caller picks `num_partitions`
/// based on cluster size and query selectivity.
pub fn split_query(
    sql: &str,
    num_partitions: u32,
    strategy: &PartitionStrategy,
) -> PartitionedPlan {
    let n = num_partitions.max(1);
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut fragments = Vec::with_capacity(n as usize);

    for i in 0..n {
        let fragment_sql = if n == 1 {
            sql.to_string()
        } else {
            match strategy {
                PartitionStrategy::HashColumn(col) => {
                    // Wrap the user query in a subquery and apply a hash
                    // filter on the chosen column. ABS keeps the modulo
                    // non-negative; the alias avoids collisions with any
                    // identifiers in the wrapped query.
                    format!(
                        "SELECT * FROM ({sql}) AS __opensnow_inner \
                         WHERE ABS(MOD(HASH({col}), {n})) = {i}"
                    )
                }
                PartitionStrategy::Replicate => sql.to_string(),
            }
        };
        fragments.push(PartitionedFragment {
            partition_id: i,
            total_partitions: n,
            sql: fragment_sql,
        });
    }

    PartitionedPlan {
        query_id,
        fragments,
    }
}

/// Merge per-partition results by concatenation.
///
/// Each input is the `Vec<RecordBatch>` returned by one worker. Empty inputs
/// are skipped. All non-empty partitions must agree on schema; otherwise
/// the merge fails with a descriptive error.
pub fn merge_results(partitions: Vec<Vec<RecordBatch>>) -> Result<Vec<RecordBatch>> {
    let mut out: Vec<RecordBatch> = Vec::new();
    let mut expected_schema = None;

    for batches in partitions {
        for batch in batches {
            match &expected_schema {
                None => expected_schema = Some(batch.schema()),
                Some(s) if s.as_ref() != batch.schema().as_ref() => {
                    bail!(
                        "scatter-gather merge: partition schema mismatch \
                         (expected {:?}, got {:?})",
                        s.fields(),
                        batch.schema().fields()
                    );
                }
                _ => {}
            }
            out.push(batch);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    #[test]
    fn split_replicate_yields_n_identical_fragments() {
        let plan = split_query("SELECT * FROM events", 4, &PartitionStrategy::Replicate);
        assert_eq!(plan.fragments.len(), 4);
        for (i, f) in plan.fragments.iter().enumerate() {
            assert_eq!(f.partition_id as usize, i);
            assert_eq!(f.total_partitions, 4);
            assert_eq!(f.sql, "SELECT * FROM events");
        }
    }

    #[test]
    fn split_hash_column_rewrites_each_fragment() {
        let plan = split_query(
            "SELECT * FROM orders",
            3,
            &PartitionStrategy::HashColumn("order_id".to_string()),
        );
        assert_eq!(plan.fragments.len(), 3);
        for (i, f) in plan.fragments.iter().enumerate() {
            assert!(
                f.sql.contains("HASH(order_id)"),
                "fragment {i} missing HASH(order_id): {}",
                f.sql
            );
            assert!(
                f.sql.contains(&format!("= {i}")),
                "fragment {i} missing partition index: {}",
                f.sql
            );
        }
    }

    #[test]
    fn split_with_zero_partitions_produces_single_passthrough() {
        let plan = split_query("SELECT 1", 0, &PartitionStrategy::Replicate);
        assert_eq!(plan.fragments.len(), 1);
        assert_eq!(plan.fragments[0].sql, "SELECT 1");
    }

    #[test]
    fn split_one_partition_skips_rewrite() {
        let plan = split_query(
            "SELECT * FROM t",
            1,
            &PartitionStrategy::HashColumn("id".into()),
        );
        // Single partition: no point hashing, return the SQL unchanged.
        assert_eq!(plan.fragments[0].sql, "SELECT * FROM t");
    }

    #[test]
    fn split_assigns_unique_query_id() {
        let p1 = split_query("SELECT 1", 2, &PartitionStrategy::Replicate);
        let p2 = split_query("SELECT 1", 2, &PartitionStrategy::Replicate);
        assert_ne!(p1.query_id, p2.query_id);
    }

    fn batch(rows: &[i64]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(rows.to_vec()))]).unwrap()
    }

    #[test]
    fn merge_concatenates_batches_from_all_partitions() {
        let merged = merge_results(vec![
            vec![batch(&[1, 2])],
            vec![batch(&[3])],
            vec![batch(&[4, 5, 6])],
        ])
        .unwrap();
        let total_rows: usize = merged.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 6);
    }

    #[test]
    fn merge_skips_empty_partitions() {
        let merged = merge_results(vec![vec![], vec![batch(&[1, 2])], vec![]]).unwrap();
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].num_rows(), 2);
    }

    #[test]
    fn merge_fails_on_schema_mismatch() {
        let s1 = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let s2 = Arc::new(Schema::new(vec![Field::new("y", DataType::Int64, false)]));
        let b1 = RecordBatch::try_new(s1, vec![Arc::new(Int64Array::from(vec![1]))]).unwrap();
        let b2 = RecordBatch::try_new(s2, vec![Arc::new(Int64Array::from(vec![2]))]).unwrap();

        let err = merge_results(vec![vec![b1], vec![b2]]).unwrap_err();
        assert!(
            err.to_string().contains("schema mismatch"),
            "expected schema mismatch error, got: {err}"
        );
    }

    #[test]
    fn merge_returns_empty_for_no_input() {
        let merged = merge_results(vec![]).unwrap();
        assert!(merged.is_empty());
    }
}
