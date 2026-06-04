//! Redis-backed worker registry.
//!
//! The in-memory [`Scheduler`](crate::scheduler::Scheduler) only knows about
//! workers connected to the same coordinator process. This module lets a
//! worker pool span multiple coordinators by storing workers in Redis:
//!
//! * `opensnow:workers` — a hash mapping `worker_id` → JSON-encoded
//!   [`WorkerRegistration`].
//! * `opensnow:worker_hb:<worker_id>` — a string with the worker's last
//!   heartbeat timestamp, written with `SET EX` so dead workers expire
//!   automatically.
//!
//! Workers call [`RedisRegistry::register`] on startup and
//! [`RedisRegistry::heartbeat`] periodically. Coordinators call
//! [`RedisRegistry::list_workers`] to enumerate the live pool.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::protocol::WorkerRegistration;

const WORKERS_KEY: &str = "opensnow:workers";
const HEARTBEAT_PREFIX: &str = "opensnow:worker_hb:";
/// Heartbeat TTL — workers must refresh within this window or they are
/// considered dead and excluded from [`list_workers`].
const DEFAULT_HEARTBEAT_TTL_SECS: u64 = 30;

/// Redis-backed worker registry. Cheap to clone (wraps a managed connection).
#[derive(Clone)]
pub struct RedisRegistry {
    conn: redis::aio::ConnectionManager,
    heartbeat_ttl_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LiveWorker {
    pub registration: WorkerRegistration,
    pub last_heartbeat: i64,
}

impl RedisRegistry {
    /// Open a Redis connection and return a registry handle.
    pub async fn connect(redis_url: &str) -> Result<Self> {
        let client = redis::Client::open(redis_url).context("invalid Redis URL")?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .context("failed to connect to Redis")?;
        Ok(Self {
            conn,
            heartbeat_ttl_secs: DEFAULT_HEARTBEAT_TTL_SECS,
        })
    }

    /// Override the heartbeat TTL (default 30 s).
    pub fn with_heartbeat_ttl(mut self, ttl_secs: u64) -> Self {
        self.heartbeat_ttl_secs = ttl_secs;
        self
    }

    /// Register a worker. The worker's metadata is stored in the
    /// `opensnow:workers` hash and a fresh heartbeat key is written.
    pub async fn register(&self, reg: &WorkerRegistration) -> Result<()> {
        use redis::AsyncCommands;
        let payload = serde_json::to_string(reg).context("serialize registration")?;
        let mut conn = self.conn.clone();
        let _: () = conn
            .hset(WORKERS_KEY, &reg.worker_id, payload)
            .await
            .context("HSET worker")?;
        self.heartbeat(&reg.worker_id).await?;
        debug!(worker_id = %reg.worker_id, "registered with Redis");
        Ok(())
    }

    /// Refresh a worker's heartbeat (caller should invoke periodically,
    /// roughly every `heartbeat_ttl_secs / 3`).
    pub async fn heartbeat(&self, worker_id: &str) -> Result<()> {
        use redis::AsyncCommands;
        let key = format!("{HEARTBEAT_PREFIX}{worker_id}");
        let now = Utc::now().timestamp();
        let mut conn = self.conn.clone();
        conn.set_ex::<_, _, ()>(&key, now, self.heartbeat_ttl_secs)
            .await
            .context("SET EX heartbeat")?;
        Ok(())
    }

    /// Remove a worker (e.g. on graceful shutdown).
    pub async fn deregister(&self, worker_id: &str) -> Result<()> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let _: () = conn.hdel(WORKERS_KEY, worker_id).await.context("HDEL")?;
        let _: () = conn
            .del(format!("{HEARTBEAT_PREFIX}{worker_id}"))
            .await
            .context("DEL heartbeat")?;
        Ok(())
    }

    /// List all live workers (those whose heartbeat key has not expired).
    pub async fn list_workers(&self) -> Result<Vec<LiveWorker>> {
        use redis::AsyncCommands;
        let mut conn = self.conn.clone();
        let entries: std::collections::HashMap<String, String> =
            conn.hgetall(WORKERS_KEY).await.context("HGETALL")?;

        let mut out = Vec::with_capacity(entries.len());
        for (worker_id, payload) in entries {
            let hb_key = format!("{HEARTBEAT_PREFIX}{worker_id}");
            let hb: Option<i64> = conn.get(&hb_key).await.context("GET heartbeat")?;
            if let Some(last_heartbeat) = hb {
                let registration: WorkerRegistration =
                    serde_json::from_str(&payload).context("deserialize registration")?;
                out.push(LiveWorker {
                    registration,
                    last_heartbeat,
                });
            }
            // Heartbeat absent — worker considered dead, skip silently.
            // We deliberately do NOT delete the workers-hash entry here so
            // that a transient Redis hiccup doesn't lose registration
            // metadata; a separate compaction job can prune.
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    //! These tests avoid spinning up a real Redis. The in-memory tests
    //! exercise the key derivation logic and the JSON shape contracts.
    //!
    //! Live-Redis behaviour is exercised by the warm-pool integration tests
    //! in `opensnow-queue`, which already require a running Redis.

    use super::*;
    use crate::protocol::WorkerRegistration;

    fn sample() -> WorkerRegistration {
        WorkerRegistration {
            worker_id: "w-1".to_string(),
            host: "10.0.0.1".to_string(),
            port: 9092,
            grpc_port: 9100,
            cpu_cores: 4,
            memory_bytes: 8_000_000_000,
            warehouse: "default".to_string(),
        }
    }

    #[test]
    fn workers_key_is_stable() {
        assert_eq!(WORKERS_KEY, "opensnow:workers");
        assert_eq!(
            format!("{HEARTBEAT_PREFIX}{}", "w-1"),
            "opensnow:worker_hb:w-1"
        );
    }

    #[test]
    fn live_worker_serializes_round_trip() {
        let lw = LiveWorker {
            registration: sample(),
            last_heartbeat: 1_700_000_000,
        };
        let json = serde_json::to_string(&lw).unwrap();
        let back: LiveWorker = serde_json::from_str(&json).unwrap();
        assert_eq!(back, lw);
    }

    #[test]
    fn registration_serializes_round_trip() {
        let reg = sample();
        let json = serde_json::to_string(&reg).unwrap();
        let back: WorkerRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(back.worker_id, reg.worker_id);
        assert_eq!(back.warehouse, reg.warehouse);
    }
}
