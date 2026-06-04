/// Job types for the OpenSnow worker queue.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type JobId = String;

/// A query job pushed to the Redis queue by a coordinator/REST handler
/// and consumed by a warm worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryJob {
    /// Unique job identifier.
    pub id: JobId,
    /// SQL statement to execute.
    pub sql: String,
    /// Warehouse label (used for routing and metrics).
    pub warehouse: String,
    /// Optional user context.
    pub user: Option<String>,
    /// Wall-clock time when the job was enqueued.
    pub enqueued_at: DateTime<Utc>,
    /// Optional client-supplied correlation id for tracing.
    pub correlation_id: Option<String>,
}

impl QueryJob {
    pub fn new(sql: impl Into<String>, warehouse: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            sql: sql.into(),
            warehouse: warehouse.into(),
            user: None,
            enqueued_at: Utc::now(),
            correlation_id: None,
        }
    }

    pub fn with_user(mut self, user: impl Into<String>) -> Self {
        self.user = Some(user.into());
        self
    }

    pub fn with_correlation_id(mut self, id: impl Into<String>) -> Self {
        self.correlation_id = Some(id.into());
        self
    }
}

/// Current lifecycle status of a job (stored separately in Redis as a hash).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running { worker_id: String },
    Done { rows: i64, duration_ms: i64 },
    Failed { error: String },
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Failed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_job_roundtrips_json() {
        let job = QueryJob::new("SELECT 1", "default").with_user("alice");
        let json = serde_json::to_string(&job).expect("serialize");
        let back: QueryJob = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(job.id, back.id);
        assert_eq!(back.user.as_deref(), Some("alice"));
    }

    #[test]
    fn job_status_terminal() {
        assert!(
            JobStatus::Done {
                rows: 1,
                duration_ms: 5
            }
            .is_terminal()
        );
        assert!(
            JobStatus::Failed {
                error: "oops".into()
            }
            .is_terminal()
        );
        assert!(!JobStatus::Queued.is_terminal());
    }
}
