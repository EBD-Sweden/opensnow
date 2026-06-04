use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::protocol::*;

/// The scheduler manages worker pool and assigns query tasks.
pub struct Scheduler {
    workers: Arc<RwLock<HashMap<String, WorkerInfo>>>,
    heartbeat_timeout_secs: i64,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            workers: Arc::new(RwLock::new(HashMap::new())),
            heartbeat_timeout_secs: 30,
        }
    }

    /// Register a new worker.
    pub async fn register_worker(&self, registration: WorkerRegistration) {
        let worker_id = registration.worker_id.clone();
        let info = WorkerInfo {
            registration,
            last_heartbeat: Utc::now().timestamp(),
            active_tasks: 0,
            status: WorkerStatus::Active,
        };

        let mut workers = self.workers.write().await;
        workers.insert(worker_id.clone(), info);
        info!(
            "Worker registered: {} ({} workers total)",
            worker_id,
            workers.len()
        );
    }

    /// Process a heartbeat from a worker.
    pub async fn heartbeat(&self, hb: Heartbeat) {
        let mut workers = self.workers.write().await;
        if let Some(worker) = workers.get_mut(&hb.worker_id) {
            worker.last_heartbeat = hb.timestamp;
            worker.active_tasks = hb.active_tasks;
        }
    }

    /// Remove a worker from the pool.
    pub async fn deregister_worker(&self, worker_id: &str) {
        let mut workers = self.workers.write().await;
        workers.remove(worker_id);
        info!(
            "Worker deregistered: {} ({} workers remaining)",
            worker_id,
            workers.len()
        );
    }

    /// Get active workers sorted by load (least loaded first).
    pub async fn get_available_workers(&self) -> Vec<WorkerInfo> {
        let now = Utc::now().timestamp();
        let workers = self.workers.read().await;

        let mut active: Vec<WorkerInfo> = workers
            .values()
            .filter(|w| {
                w.status == WorkerStatus::Active
                    && (now - w.last_heartbeat) < self.heartbeat_timeout_secs
            })
            .cloned()
            .collect();

        // Sort by active_tasks ascending (least loaded first)
        active.sort_by_key(|w| w.active_tasks);
        active
    }

    /// Pick the best N workers for a query (round-robin by load).
    pub async fn select_workers(&self, count: usize) -> Vec<WorkerInfo> {
        let available = self.get_available_workers().await;
        if available.is_empty() {
            warn!("No workers available for query execution");
            return vec![];
        }
        available.into_iter().take(count).collect()
    }

    /// Get cluster state for monitoring.
    pub async fn cluster_state(&self) -> ClusterState {
        let workers = self.workers.read().await;
        let worker_infos: Vec<WorkerInfo> = workers.values().cloned().collect();
        let active_tasks: u32 = worker_infos.iter().map(|w| w.active_tasks).sum();

        ClusterState {
            workers: worker_infos,
            active_queries: active_tasks,
            pending_queries: 0,
        }
    }

    /// Mark dead workers (no heartbeat within timeout).
    pub async fn check_health(&self) {
        let now = Utc::now().timestamp();
        let mut workers = self.workers.write().await;

        for worker in workers.values_mut() {
            if worker.status == WorkerStatus::Active
                && (now - worker.last_heartbeat) > self.heartbeat_timeout_secs
            {
                warn!(
                    "Worker {} is dead (no heartbeat for {}s)",
                    worker.registration.worker_id,
                    now - worker.last_heartbeat
                );
                worker.status = WorkerStatus::Dead;
            }
        }

        // Remove dead workers
        workers.retain(|_, w| w.status != WorkerStatus::Dead);
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_select() {
        let scheduler = Scheduler::new();

        scheduler
            .register_worker(WorkerRegistration {
                worker_id: "w1".to_string(),
                host: "localhost".to_string(),
                port: 9092,
                grpc_port: 9100,
                cpu_cores: 4,
                memory_bytes: 8_000_000_000,
                warehouse: "default".to_string(),
            })
            .await;

        scheduler
            .register_worker(WorkerRegistration {
                worker_id: "w2".to_string(),
                host: "localhost".to_string(),
                port: 9093,
                grpc_port: 9101,
                cpu_cores: 8,
                memory_bytes: 16_000_000_000,
                warehouse: "default".to_string(),
            })
            .await;

        let workers = scheduler.select_workers(2).await;
        assert_eq!(workers.len(), 2);

        let state = scheduler.cluster_state().await;
        assert_eq!(state.workers.len(), 2);
    }
}
