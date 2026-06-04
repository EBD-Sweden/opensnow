use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use arrow_flight::Action;
use arrow_flight::flight_service_client::FlightServiceClient;
use chrono::Utc;
use opensnow_core::OpenSnowEngine;
use tracing::{error, info};

use crate::protocol::*;

/// A worker node that executes query tasks assigned by the coordinator.
pub struct Worker {
    worker_id: String,
    engine: Arc<OpenSnowEngine>,
    coordinator_url: String,
    host: String,
    port: u16,
    grpc_port: u16,
    warehouse: String,
}

impl Worker {
    pub fn new(
        engine: Arc<OpenSnowEngine>,
        coordinator_url: String,
        host: String,
        port: u16,
        grpc_port: u16,
        warehouse: String,
    ) -> Self {
        Self {
            worker_id: uuid::Uuid::new_v4().to_string(),
            engine,
            coordinator_url,
            host,
            port,
            grpc_port,
            warehouse,
        }
    }

    /// Register with the coordinator and start the heartbeat loop.
    ///
    /// Registration is deferred until `ready_flag` is `true` so that the
    /// coordinator only receives workers that have passed the warm-up check
    /// (catalog loaded + test query successful).  Pass `None` to skip the
    /// readiness gate (dev/test mode).
    pub async fn start_with_ready_flag(
        &self,
        ready_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ) -> Result<()> {
        info!(
            "Worker {} starting, connecting to coordinator at {}",
            self.worker_id, self.coordinator_url
        );

        if let Some(flag) = ready_flag {
            wait_for_ready_flag(&flag, &self.worker_id).await;
        }

        self.register().await?;

        let worker_id = self.worker_id.clone();
        let coordinator_url = self.coordinator_url.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;
                if let Err(e) = send_heartbeat(&coordinator_url, &worker_id).await {
                    error!("Heartbeat failed: {}", e);
                }
            }
        });

        info!("Worker {} registered and heartbeat started", self.worker_id);
        Ok(())
    }

    /// Convenience wrapper — no readiness gate (dev/test).
    pub async fn start(&self) -> Result<()> {
        self.start_with_ready_flag(None).await
    }

    async fn register(&self) -> Result<()> {
        let cpu_cores = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4);

        let reg = WorkerRegistration {
            worker_id: self.worker_id.clone(),
            host: self.host.clone(),
            port: self.port,
            grpc_port: self.grpc_port,
            cpu_cores,
            memory_bytes: 8_000_000_000, // 8GB default
            warehouse: self.warehouse.clone(),
        };

        let mut client = FlightServiceClient::connect(self.coordinator_url.clone()).await?;
        let action = Action {
            r#type: "register_worker".to_string(),
            body: bytes::Bytes::from(serde_json::to_vec(&reg)?),
        };
        client.do_action(tonic::Request::new(action)).await?;
        info!("Registered with coordinator as {}", self.worker_id);
        Ok(())
    }

    /// Execute a query task locally.
    pub async fn execute_task(&self, task: &QueryTask) -> Result<TaskResult> {
        info!("Executing task {} (query {})", task.task_id, task.query_id);

        match self.engine.execute_sql(&task.sql_fragment).await {
            Ok(batches) => {
                let rows: u64 = batches.iter().map(|b| b.num_rows() as u64).sum();
                Ok(TaskResult {
                    task_id: task.task_id.clone(),
                    query_id: task.query_id.clone(),
                    status: TaskStatus::Completed,
                    rows_produced: rows,
                    bytes_processed: 0,
                    error_message: None,
                })
            }
            Err(e) => Ok(TaskResult {
                task_id: task.task_id.clone(),
                query_id: task.query_id.clone(),
                status: TaskStatus::Failed,
                rows_produced: 0,
                bytes_processed: 0,
                error_message: Some(e.to_string()),
            }),
        }
    }

    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }
}

/// Poll `flag` every 200 ms and return once it is set.
///
/// Used by both `start_with_ready_flag` (coordinator registration) and
/// the warm-pool worker so that neither the coordinator nor the Redis poll
/// loop sees the worker until the readiness probe has passed.
pub async fn wait_for_ready_flag(
    flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker_id: &str,
) {
    if !flag.load(std::sync::atomic::Ordering::Acquire) {
        info!("Worker {} waiting for readiness probe...", worker_id);
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if flag.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
        }
        info!(
            "Worker {} is warm — registering with coordinator",
            worker_id
        );
    }
}

async fn send_heartbeat(coordinator_url: &str, worker_id: &str) -> Result<()> {
    let hb = Heartbeat {
        worker_id: worker_id.to_string(),
        active_tasks: 0,
        cpu_usage: 0.0,
        memory_used_bytes: 0,
        cache_hit_ratio: 0.0,
        timestamp: Utc::now().timestamp(),
    };

    let mut client = FlightServiceClient::connect(coordinator_url.to_string()).await?;
    let action = Action {
        r#type: "heartbeat".to_string(),
        body: bytes::Bytes::from(serde_json::to_vec(&hb)?),
    };
    client.do_action(tonic::Request::new(action)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn wait_for_ready_flag_returns_immediately_when_already_set() {
        let flag = Arc::new(AtomicBool::new(true));
        // Should return without blocking — if it hangs the test times out.
        wait_for_ready_flag(&flag, "test-worker").await;
        assert!(flag.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn wait_for_ready_flag_waits_until_flag_is_set() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            flag_clone.store(true, Ordering::Release);
        });

        wait_for_ready_flag(&flag, "test-worker").await;
        assert!(flag.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn wait_for_ready_flag_skipped_when_none_passed() {
        // Simulates dev/test path: start() passes None so no wait occurs.
        // We verify the flag-skip branch compiles and runs correctly by
        // calling the equivalent logic directly (Option::None branch).
        let ready_flag: Option<Arc<AtomicBool>> = None;
        if let Some(flag) = ready_flag {
            wait_for_ready_flag(&flag, "test-worker").await;
        }
        // reaching here without hanging = pass
    }
}
