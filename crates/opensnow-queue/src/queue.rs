/// Redis-backed job queue.
///
/// Push end (coordinator / REST handler):
///   `push(job)` → LPUSH opensnow:queue:<warehouse>
///
/// Pop end (warm worker):
///   `pop(warehouse, timeout_secs)` → BRPOP (blocking)
///
/// Job status is stored in a separate Redis hash:
///   HSET opensnow:status:<job_id>  status  <json>
use anyhow::{Context, Result};
use redis::AsyncCommands;
use redis::aio::ConnectionManager;
use tracing::debug;

use crate::job::{JobId, JobStatus, QueryJob};

const QUEUE_PREFIX: &str = "opensnow:queue:";
const STATUS_PREFIX: &str = "opensnow:status:";
/// Default job status TTL — keep results for 1 hour.
const STATUS_TTL_SECS: u64 = 3600;

#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// Redis URL, e.g. `redis://127.0.0.1:6379`.
    pub redis_url: String,
    /// Default BRPOP timeout in seconds (0 = block forever).
    pub pop_timeout_secs: u64,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            redis_url: std::env::var("OPENSNOW_REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            pop_timeout_secs: 5,
        }
    }
}

/// `JobQueue` is `Clone` so it can be shared across handlers and workers.
#[derive(Clone)]
pub struct JobQueue {
    conn: ConnectionManager,
    pub config: QueueConfig,
}

impl JobQueue {
    /// Connect to Redis and return a ready queue handle.
    pub async fn connect(config: QueueConfig) -> Result<Self> {
        let client = redis::Client::open(config.redis_url.as_str()).context("invalid Redis URL")?;
        let conn = ConnectionManager::new(client)
            .await
            .context("failed to connect to Redis")?;
        Ok(Self { conn, config })
    }

    // ── Producer side ─────────────────────────────────────────────────────────

    /// Push a job onto the warehouse queue.
    /// Returns the queue length after push.
    pub async fn push(&self, job: &QueryJob) -> Result<i64> {
        let key = format!("{}{}", QUEUE_PREFIX, job.warehouse);
        let payload = serde_json::to_string(job).context("serialize job")?;
        let mut conn = self.conn.clone();
        let len: i64 = conn.lpush(&key, payload).await.context("LPUSH")?;
        debug!("queued job {} on {} (queue len={})", job.id, key, len);
        // Set initial status.
        self.set_status(&job.id, &JobStatus::Queued).await?;
        Ok(len)
    }

    // ── Consumer side ─────────────────────────────────────────────────────────

    /// Block until a job arrives on any of the given warehouse queues,
    /// or until `timeout_secs` elapses (returns `None` on timeout).
    pub async fn pop(&self, warehouses: &[&str], timeout_secs: u64) -> Result<Option<QueryJob>> {
        if warehouses.is_empty() {
            return Ok(None);
        }
        let keys: Vec<String> = warehouses
            .iter()
            .map(|w| format!("{}{}", QUEUE_PREFIX, w))
            .collect();
        let key_refs: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();

        let mut conn = self.conn.clone();
        let result: Option<(String, String)> = conn
            .brpop(&key_refs[..], timeout_secs as f64)
            .await
            .context("BRPOP")?;

        match result {
            None => Ok(None),
            Some((_key, payload)) => {
                let job: QueryJob = serde_json::from_str(&payload).context("deserialize job")?;
                debug!("dequeued job {}", job.id);
                Ok(Some(job))
            }
        }
    }

    // ── Status tracking ───────────────────────────────────────────────────────

    /// Write job status to Redis (expires after STATUS_TTL_SECS).
    pub async fn set_status(&self, job_id: &JobId, status: &JobStatus) -> Result<()> {
        let key = format!("{}{}", STATUS_PREFIX, job_id);
        let payload = serde_json::to_string(status).context("serialize status")?;
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(&key, payload, STATUS_TTL_SECS)
            .await
            .context("SET EX status")?;
        Ok(())
    }

    /// Read job status from Redis. Returns `None` if not found / expired.
    pub async fn get_status(&self, job_id: &JobId) -> Result<Option<JobStatus>> {
        let key = format!("{}{}", STATUS_PREFIX, job_id);
        let mut conn = self.conn.clone();
        let payload: Option<String> = conn.get(&key).await.context("GET status")?;
        match payload {
            None => Ok(None),
            Some(s) => {
                let status: JobStatus = serde_json::from_str(&s).context("deserialize status")?;
                Ok(Some(status))
            }
        }
    }

    /// Return the current depth of a warehouse queue (non-blocking).
    pub async fn queue_depth(&self, warehouse: &str) -> Result<i64> {
        let key = format!("{}{}", QUEUE_PREFIX, warehouse);
        let mut conn = self.conn.clone();
        let len: i64 = conn.llen(&key).await.context("LLEN")?;
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit test that exercises the serialization/deserialization path
    /// without needing a live Redis connection.
    #[test]
    fn queue_key_format() {
        let w = "analytics";
        let key = format!("{}{}", QUEUE_PREFIX, w);
        assert_eq!(key, "opensnow:queue:analytics");
    }

    #[test]
    fn status_key_format() {
        let id = "abc-123";
        let key = format!("{}{}", STATUS_PREFIX, id);
        assert_eq!(key, "opensnow:status:abc-123");
    }

    #[test]
    fn job_status_json_roundtrip() {
        let status = JobStatus::Done {
            rows: 42,
            duration_ms: 10,
        };
        let s = serde_json::to_string(&status).unwrap();
        let back: JobStatus = serde_json::from_str(&s).unwrap();
        assert_eq!(back, status);
    }
}
