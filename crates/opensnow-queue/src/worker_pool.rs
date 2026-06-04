/// Warm worker pool management.
///
/// A warm worker:
///  1. Starts up, loads the engine (catalog init, DataFusion context).
///  2. Passes a readiness probe (`/ready` returns 200).
///  3. Begins polling the Redis queue via BRPOP.
///  4. Executes jobs, writes status back to Redis.
///
/// `WarmWorkerHandle` is the in-process representation of a running worker.
/// The Kubernetes warm pool is managed separately via a KEDA ScaledObject
/// (see `deploy/keda-warm-pool.yaml`).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use anyhow::Result;
use tokio::sync::Notify;
use tracing::{error, info, warn};

use crate::job::{JobStatus, QueryJob};
use crate::queue::JobQueue;

#[derive(Debug, Clone)]
pub struct WarmPoolConfig {
    /// Warehouse queues this worker should poll.
    pub warehouses: Vec<String>,
    /// How long to block on BRPOP before looping (seconds).
    pub pop_timeout_secs: u64,
    /// Worker identity string (pod name / hostname).
    pub worker_id: String,
    /// If set, the poll loop will not start until this flag is `true`.
    ///
    /// Wire this to the same `ReadyFlag` used by `serve_readiness_probe` so
    /// that job acceptance is gated on the same warm-up check that the HTTP
    /// probe and coordinator registration use.  `None` = start immediately
    /// (dev/test mode).
    pub ready_flag: Option<Arc<AtomicBool>>,
}

impl Default for WarmPoolConfig {
    fn default() -> Self {
        let worker_id =
            std::env::var("OPENSNOW_WORKER_ID").unwrap_or_else(|_| "worker-local".to_string());
        Self {
            warehouses: vec!["default".to_string()],
            pop_timeout_secs: 5,
            worker_id,
            ready_flag: None,
        }
    }
}

/// A handle to a running warm worker poll loop.
///
/// Drop the handle to request a graceful shutdown.
pub struct WarmWorkerHandle {
    shutdown: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl WarmWorkerHandle {
    /// Signal the worker to stop after finishing the current job.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.notify.notify_one();
    }
}

impl Drop for WarmWorkerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn a warm worker poll loop.
///
/// The `executor` closure is called for each job and should return the number
/// of rows produced (or an error).  In production this calls into
/// `EngineHandle::execute_sql`; in tests it can be a stub.
pub fn spawn_warm_worker<F, Fut>(
    queue: JobQueue,
    config: WarmPoolConfig,
    executor: F,
) -> WarmWorkerHandle
where
    F: Fn(QueryJob) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<i64>> + Send + 'static,
{
    let shutdown = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());

    let shutdown_clone = shutdown.clone();
    let notify_clone = notify.clone();

    tokio::spawn(async move {
        // Gate on readiness: don't accept jobs until warm-up has passed.
        if let Some(ref flag) = config.ready_flag
            && !flag.load(Ordering::Acquire)
        {
            info!(worker_id = %config.worker_id, "waiting for warm-up before polling queue");
            while !flag.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            info!(worker_id = %config.worker_id, "warm-up done — starting queue poll");
        }

        let warehouses: Vec<&str> = config.warehouses.iter().map(|s| s.as_str()).collect();
        info!(
            worker_id = %config.worker_id,
            warehouses = ?warehouses,
            "warm worker started"
        );

        loop {
            if shutdown_clone.load(Ordering::Acquire) {
                info!(worker_id = %config.worker_id, "warm worker shutting down");
                break;
            }

            // BRPOP with timeout — returns None on timeout so we can re-check shutdown.
            let job = match queue.pop(&warehouses, config.pop_timeout_secs).await {
                Ok(Some(j)) => j,
                Ok(None) => continue, // timeout — loop again
                Err(e) => {
                    warn!(worker_id = %config.worker_id, "queue pop error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            };

            let job_id = job.id.clone();
            info!(
                worker_id = %config.worker_id,
                job_id = %job_id,
                warehouse = %job.warehouse,
                "executing job"
            );

            // Mark running.
            if let Err(e) = queue
                .set_status(
                    &job_id,
                    &JobStatus::Running {
                        worker_id: config.worker_id.clone(),
                    },
                )
                .await
            {
                warn!("failed to set running status for {}: {}", job_id, e);
            }

            let start = Instant::now();
            let result = executor(job).await;
            let duration_ms = start.elapsed().as_millis() as i64;

            let final_status = match result {
                Ok(rows) => {
                    info!(
                        job_id = %job_id,
                        rows,
                        duration_ms,
                        "job completed"
                    );
                    JobStatus::Done { rows, duration_ms }
                }
                Err(e) => {
                    error!(job_id = %job_id, "job failed: {}", e);
                    JobStatus::Failed {
                        error: e.to_string(),
                    }
                }
            };

            if let Err(e) = queue.set_status(&job_id, &final_status).await {
                warn!("failed to write final status for {}: {}", job_id, e);
            }
        }

        info!(worker_id = %config.worker_id, "warm worker stopped");
    });

    WarmWorkerHandle {
        shutdown,
        notify: notify_clone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warm_pool_config_defaults() {
        let cfg = WarmPoolConfig::default();
        assert_eq!(cfg.warehouses, vec!["default"]);
        assert_eq!(cfg.pop_timeout_secs, 5);
        assert!(cfg.ready_flag.is_none());
    }

    #[tokio::test]
    async fn ready_flag_set_to_true_does_not_block() {
        let flag = Arc::new(AtomicBool::new(true));
        // Simulate the gate logic directly — should return without sleeping.
        if !flag.load(Ordering::Acquire) {
            while !flag.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
        assert!(flag.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn ready_flag_false_then_true_unblocks() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            flag_clone.store(true, Ordering::Release);
        });

        // Simulate the gate logic from spawn_warm_worker.
        while !flag.load(Ordering::Acquire) {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(flag.load(Ordering::Acquire));
    }
}
