//! Coordinator-side scatter-gather query executor.
//!
//! # Protocol
//!
//! 1. Client sends `SQL` to the coordinator.
//! 2. Coordinator calls [`partitioner::split_query`](crate::partitioner::split_query)
//!    to produce N [`PartitionedFragment`](crate::partitioner::PartitionedFragment)s.
//! 3. For each fragment, the coordinator picks a [`WorkerExecutor`] and calls
//!    `execute_fragment` concurrently. Each executor is responsible for
//!    transporting the fragment to a worker (Arrow Flight, in-process, …)
//!    and returning the per-partition `Vec<RecordBatch>`.
//! 4. The coordinator merges the partial results with
//!    [`merge_results`](crate::partitioner::merge_results) and returns the
//!    final batches to the client.
//!
//! Failures: if any executor returns an error, the whole query fails — there
//! is no per-partition retry yet. A future iteration can add retry / fallback
//! to local execution.

use std::sync::Arc;

use anyhow::{Context, Result};
use arrow::array::RecordBatch;
use async_trait::async_trait;
use futures::future::try_join_all;
use opensnow_core::EngineHandle;
use tracing::{debug, info};

use crate::partitioner::{PartitionStrategy, PartitionedFragment, merge_results, split_query};

/// Anything that can run a single per-partition query fragment.
///
/// The trait is async + `Send + Sync` because the executor lives behind an
/// [`Arc`] and is called concurrently from the coordinator's gather step.
#[async_trait]
pub trait WorkerExecutor: Send + Sync {
    /// Execute one fragment and return the resulting Arrow batches.
    async fn execute_fragment(&self, fragment: &PartitionedFragment) -> Result<Vec<RecordBatch>>;

    /// Identifier for logs / metrics.
    fn label(&self) -> &str {
        "worker"
    }
}

/// Run a fragment locally on the coordinator's [`EngineHandle`].
///
/// Useful for tests, single-node fallback, and the in-process path used by
/// the e2e tests where there are no remote workers.
pub struct LocalWorkerExecutor {
    handle: EngineHandle,
    label: String,
}

impl LocalWorkerExecutor {
    pub fn new(handle: EngineHandle) -> Self {
        Self {
            handle,
            label: "local".to_string(),
        }
    }

    pub fn with_label(handle: EngineHandle, label: impl Into<String>) -> Self {
        Self {
            handle,
            label: label.into(),
        }
    }
}

#[async_trait]
impl WorkerExecutor for LocalWorkerExecutor {
    async fn execute_fragment(&self, fragment: &PartitionedFragment) -> Result<Vec<RecordBatch>> {
        debug!(
            partition = fragment.partition_id,
            total = fragment.total_partitions,
            "local executor running fragment"
        );
        self.handle
            .execute_sql(&fragment.sql)
            .await
            .with_context(|| {
                format!(
                    "fragment {}/{} failed",
                    fragment.partition_id, fragment.total_partitions
                )
            })
    }

    fn label(&self) -> &str {
        &self.label
    }
}

/// Coordinates a scatter-gather query against a set of [`WorkerExecutor`]s.
pub struct DistributedExecutor {
    workers: Vec<Arc<dyn WorkerExecutor>>,
    /// Default partitioning when the caller does not supply one.
    default_strategy: PartitionStrategy,
}

impl DistributedExecutor {
    /// Build an executor over the given worker pool. Must contain at least
    /// one worker; otherwise [`execute`] returns an error.
    pub fn new(workers: Vec<Arc<dyn WorkerExecutor>>) -> Self {
        Self {
            workers,
            default_strategy: PartitionStrategy::Replicate,
        }
    }

    /// Override the default partitioning strategy.
    pub fn with_strategy(mut self, strategy: PartitionStrategy) -> Self {
        self.default_strategy = strategy;
        self
    }

    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    /// Run `sql` distributed across the configured workers and merge results.
    ///
    /// One fragment is created per worker. Workers are addressed in order
    /// (worker `i` runs partition `i`). The coordinator awaits all fragments
    /// concurrently before merging.
    pub async fn execute(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        self.execute_with_strategy(sql, &self.default_strategy)
            .await
    }

    /// Run `sql` with an explicit [`PartitionStrategy`].
    pub async fn execute_with_strategy(
        &self,
        sql: &str,
        strategy: &PartitionStrategy,
    ) -> Result<Vec<RecordBatch>> {
        if self.workers.is_empty() {
            anyhow::bail!("DistributedExecutor: no workers configured");
        }

        let n = self.workers.len() as u32;
        let plan = split_query(sql, n, strategy);
        info!(
            query_id = %plan.query_id,
            partitions = plan.fragments.len(),
            "scattering query across {} workers",
            self.workers.len()
        );

        // Dispatch concurrently — one future per (fragment, worker) pair.
        let futures = plan
            .fragments
            .iter()
            .zip(self.workers.iter())
            .map(|(fragment, worker)| {
                let worker = worker.clone();
                let fragment = fragment.clone();
                async move {
                    let label = worker.label().to_string();
                    let result = worker.execute_fragment(&fragment).await;
                    debug!(
                        worker = %label,
                        partition = fragment.partition_id,
                        ok = result.is_ok(),
                        "fragment finished"
                    );
                    result
                }
            });

        let partial: Vec<Vec<RecordBatch>> = try_join_all(futures).await?;
        let total_rows: usize = partial.iter().flatten().map(|b| b.num_rows()).sum();
        info!(
            query_id = %plan.query_id,
            total_rows,
            "gather complete; merging"
        );

        merge_results(partial)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use arrow::datatypes::{DataType, Field, Schema};

    /// A test executor that returns a fixed batch for every fragment, and
    /// records the SQL it was asked to run for assertions.
    struct CountingExecutor {
        rows_per_fragment: i64,
        seen: tokio::sync::Mutex<Vec<String>>,
    }

    impl CountingExecutor {
        fn new(rows_per_fragment: i64) -> Self {
            Self {
                rows_per_fragment,
                seen: tokio::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl WorkerExecutor for CountingExecutor {
        async fn execute_fragment(
            &self,
            fragment: &PartitionedFragment,
        ) -> Result<Vec<RecordBatch>> {
            self.seen.lock().await.push(fragment.sql.clone());
            let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
            let values: Vec<i64> = (0..self.rows_per_fragment)
                .map(|i| (fragment.partition_id as i64) * 100 + i)
                .collect();
            let batch = RecordBatch::try_new(schema, vec![Arc::new(Int64Array::from(values))])?;
            Ok(vec![batch])
        }

        fn label(&self) -> &str {
            "counting"
        }
    }

    #[tokio::test]
    async fn execute_dispatches_to_each_worker_and_merges() {
        let workers: Vec<Arc<dyn WorkerExecutor>> = (0..3)
            .map(|_| Arc::new(CountingExecutor::new(2)) as Arc<dyn WorkerExecutor>)
            .collect();

        let exec = DistributedExecutor::new(workers);
        let batches = exec.execute("SELECT * FROM t").await.unwrap();

        // 3 workers × 2 rows each = 6 rows total
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 6);
    }

    #[tokio::test]
    async fn execute_with_no_workers_errors() {
        let exec = DistributedExecutor::new(Vec::new());
        let err = exec.execute("SELECT 1").await.unwrap_err();
        assert!(err.to_string().contains("no workers"));
    }

    #[tokio::test]
    async fn execute_with_strategy_overrides_default() {
        let counting = Arc::new(CountingExecutor::new(0));
        let workers: Vec<Arc<dyn WorkerExecutor>> = vec![
            counting.clone() as Arc<dyn WorkerExecutor>,
            counting.clone() as Arc<dyn WorkerExecutor>,
        ];
        let exec = DistributedExecutor::new(workers);

        exec.execute_with_strategy(
            "SELECT * FROM orders",
            &PartitionStrategy::HashColumn("order_id".to_string()),
        )
        .await
        .unwrap();

        let seen = counting.seen.lock().await;
        // Both fragments should have been dispatched and rewritten with the
        // HASH(order_id) predicate.
        assert_eq!(seen.len(), 2);
        for sql in seen.iter() {
            assert!(
                sql.contains("HASH(order_id)"),
                "fragment SQL missing HASH(order_id): {sql}"
            );
        }
    }
}
