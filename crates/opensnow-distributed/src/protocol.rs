use serde::{Deserialize, Serialize};

/// Worker registration message sent when a worker joins the cluster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerRegistration {
    pub worker_id: String,
    pub host: String,
    pub port: u16,
    pub grpc_port: u16,
    pub cpu_cores: u32,
    pub memory_bytes: u64,
    pub warehouse: String,
}

/// Query task sent from coordinator to worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryTask {
    pub task_id: String,
    pub query_id: String,
    pub stage_id: u32,
    pub partition_id: u32,
    pub sql_fragment: String,
    pub input_partitions: Vec<String>,
}

/// Task result from worker back to coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub query_id: String,
    pub status: TaskStatus,
    pub rows_produced: u64,
    pub bytes_processed: u64,
    pub error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// Heartbeat from worker to coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub worker_id: String,
    pub active_tasks: u32,
    pub cpu_usage: f32,
    pub memory_used_bytes: u64,
    pub cache_hit_ratio: f32,
    pub timestamp: i64,
}

/// Cluster state as seen by the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterState {
    pub workers: Vec<WorkerInfo>,
    pub active_queries: u32,
    pub pending_queries: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerInfo {
    pub registration: WorkerRegistration,
    pub last_heartbeat: i64,
    pub active_tasks: u32,
    pub status: WorkerStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WorkerStatus {
    Active,
    Draining,
    Dead,
}
