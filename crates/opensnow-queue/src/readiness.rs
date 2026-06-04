/// Warm-worker readiness probe HTTP server.
///
/// Listens on `OPENSNOW_READY_PORT` (default 8091) and exposes:
///   GET /ready  → 200 {"status":"ready"} when the worker has passed its
///                    self-check (engine loaded + test query succeeded)
///   GET /live   → 200 always (liveness — process is running)
///
/// Kubernetes probes:
///   readinessProbe: httpGet path: /ready port: 8091 initialDelaySeconds: 3
///   livenessProbe:  httpGet path: /live  port: 8091
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use opensnow_core::EngineHandle;
use serde_json::{Value, json};
use tracing::info;

pub type ReadyFlag = Arc<AtomicBool>;

/// Spawn the readiness probe HTTP server in a background task.
///
/// `ready_flag`: set to `true` once the worker has finished warm-up.
pub async fn serve_readiness_probe(ready_flag: ReadyFlag, port: u16) -> Result<()> {
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let app = Router::new()
        .route("/ready", get(ready_handler))
        .route("/live", get(live_handler))
        .with_state(ready_flag);
    info!("readiness probe listening on http://0.0.0.0:{}", port);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ready_handler(State(flag): State<ReadyFlag>) -> (StatusCode, Json<Value>) {
    if flag.load(Ordering::Acquire) {
        (StatusCode::OK, Json(json!({ "status": "ready" })))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "warming_up" })),
        )
    }
}

async fn live_handler() -> Json<Value> {
    Json(json!({ "status": "alive" }))
}

/// Helper: run warm-up checks and set the ready flag.
///
/// Calls `check_fn`; if it returns `Ok(())` the flag is set and the worker
/// starts accepting jobs.  On failure it loops with a backoff.
pub async fn run_warmup<F, Fut>(ready_flag: ReadyFlag, check_fn: F)
where
    F: Fn() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send,
{
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match check_fn().await {
            Ok(()) => {
                info!("warm-up check passed (attempt {})", attempt);
                ready_flag.store(true, Ordering::Release);
                break;
            }
            Err(e) => {
                let wait = std::cmp::min(2u64.pow(attempt), 30);
                tracing::warn!(
                    "warm-up attempt {} failed: {} — retrying in {}s",
                    attempt,
                    e,
                    wait
                );
                tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            }
        }
    }
}

/// Run the two mandatory warm-up checks against a live `EngineHandle`:
///
/// 1. Catalog reachable — calls `list_tables("opensnow", "public")` and succeeds
///    if the call returns without error (empty result is fine for a fresh node).
/// 2. Query engine functional — executes `SELECT 1` and verifies it returns at
///    least one `RecordBatch` with one row.
///
/// Intended to be passed as the `check_fn` closure to `run_warmup`:
///
/// ```ignore
/// let handle: EngineHandle = …;
/// run_warmup(ready_flag.clone(), move || {
///     let h = handle.clone();
///     async move { engine_warmup_check(&h).await }
/// }).await;
/// ```
pub async fn engine_warmup_check(engine: &EngineHandle) -> Result<()> {
    // 1. Catalog probe — verify the metadata store is online and readable.
    engine
        .list_tables("opensnow", "public")
        .await
        .map_err(|e| anyhow::anyhow!("catalog check failed: {}", e))?;
    info!("readiness: catalog reachable");

    // 2. Query engine probe — SELECT 1 must return exactly one row.
    let batches = engine
        .execute_sql("SELECT 1")
        .await
        .map_err(|e| anyhow::anyhow!("SELECT 1 failed: {}", e))?;

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    if total_rows == 0 {
        anyhow::bail!("SELECT 1 returned 0 rows");
    }
    info!("readiness: SELECT 1 passed ({} row(s))", total_rows);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ready_flag_starts_false() {
        let flag: ReadyFlag = Arc::new(AtomicBool::new(false));
        assert!(!flag.load(Ordering::Acquire));
        flag.store(true, Ordering::Release);
        assert!(flag.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn engine_warmup_check_passes() {
        use opensnow_core::{EngineConfig, OpenSnowEngine};

        let engine = OpenSnowEngine::from_config_and_catalog(EngineConfig::default(), ":memory:");
        let handle = EngineHandle::spawn(engine);

        engine_warmup_check(&handle)
            .await
            .expect("warmup check should pass against a fresh in-memory engine");
    }
}
