//! In-memory streaming ingest buffer with periodic Parquet compaction.
//!
//! Rows pushed via `POST /api/v1/ingest/batch` accumulate per
//! `(tenant_id, table_name)` key in memory. A background task drains the
//! buffer to a Parquet file every [`FLUSH_INTERVAL`] or whenever the
//! aggregate buffer size exceeds [`FLUSH_THRESHOLD`] rows — whichever fires
//! first. Each flush registers the resulting Parquet file with the engine so
//! subsequent SQL queries see the new rows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::json::ReaderBuilder;
use chrono::{DateTime, Utc};
use opensnow_core::EngineHandle;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Flush whenever the aggregate buffer hits this many rows.
pub const FLUSH_THRESHOLD: usize = 10_000;
/// Flush whenever this much wall-clock time has elapsed since the last flush.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Public shared handle to the buffer. Cloning is cheap (Arc clone).
pub type SharedBuffer = Arc<IngestBuffer>;

/// Table key combining tenant and unqualified table name.
type Key = (String, String);

#[derive(Default)]
struct BufferState {
    buffers: HashMap<Key, Vec<Value>>,
    last_flush: Option<DateTime<Utc>>,
    total_rows_flushed: u64,
    total_files_flushed: u64,
}

/// Shared streaming ingest buffer.
pub struct IngestBuffer {
    state: Mutex<BufferState>,
    warehouse_path: PathBuf,
    flush_threshold: usize,
    flush_interval: Duration,
}

/// Snapshot of buffer stats used by the `/api/v1/ingest/status` endpoint and
/// the unit tests.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BufferStatus {
    pub buffered_rows: usize,
    pub last_flush: Option<String>,
    pub total_rows_flushed: u64,
    pub total_files_flushed: u64,
    pub flush_threshold_rows: usize,
    pub flush_interval_seconds: u64,
}

/// Description of a Parquet file produced by a flush.
#[derive(Debug, Clone)]
pub struct FlushedTable {
    pub tenant_id: String,
    pub table_name: String,
    /// DataFusion table name used when registering. We prefix with tenant id
    /// so that two tenants ingesting into a same-named table do not clobber
    /// each other inside the shared session.
    pub registered_as: String,
    pub parquet_path: PathBuf,
    pub rows: usize,
}

impl IngestBuffer {
    pub fn new(warehouse_path: impl Into<PathBuf>) -> Self {
        Self::with_config(warehouse_path, FLUSH_THRESHOLD, FLUSH_INTERVAL)
    }

    pub fn with_config(
        warehouse_path: impl Into<PathBuf>,
        flush_threshold: usize,
        flush_interval: Duration,
    ) -> Self {
        Self {
            state: Mutex::new(BufferState::default()),
            warehouse_path: warehouse_path.into(),
            flush_threshold,
            flush_interval,
        }
    }

    pub fn shared(warehouse_path: impl Into<PathBuf>) -> SharedBuffer {
        Arc::new(Self::new(warehouse_path))
    }

    /// Append rows to the buffer for `(tenant_id, table_name)`. Returns the
    /// new aggregate buffer size.
    pub async fn push(&self, tenant_id: &str, table: &str, rows: Vec<Value>) -> usize {
        let mut state = self.state.lock().await;
        let entry = state
            .buffers
            .entry((tenant_id.to_string(), table.to_string()))
            .or_default();
        entry.extend(rows);
        state.buffers.values().map(|v| v.len()).sum()
    }

    pub async fn buffered_rows(&self) -> usize {
        let state = self.state.lock().await;
        state.buffers.values().map(|v| v.len()).sum()
    }

    pub async fn status(&self) -> BufferStatus {
        let state = self.state.lock().await;
        BufferStatus {
            buffered_rows: state.buffers.values().map(|v| v.len()).sum(),
            last_flush: state.last_flush.map(|t| t.to_rfc3339()),
            total_rows_flushed: state.total_rows_flushed,
            total_files_flushed: state.total_files_flushed,
            flush_threshold_rows: self.flush_threshold,
            flush_interval_seconds: self.flush_interval.as_secs(),
        }
    }

    /// True when the buffer is due for a flush — either rows hit the
    /// threshold or the configured interval has elapsed.
    pub async fn should_flush(&self) -> bool {
        let state = self.state.lock().await;
        let total: usize = state.buffers.values().map(|v| v.len()).sum();
        if total == 0 {
            return false;
        }
        if total >= self.flush_threshold {
            return true;
        }
        match state.last_flush {
            Some(t) => Utc::now()
                .signed_duration_since(t)
                .to_std()
                .map(|d| d >= self.flush_interval)
                .unwrap_or(true),
            None => true,
        }
    }

    /// Drain the buffer to disk without registering with an engine. Returns
    /// the per-table flushed descriptors. Used by the background compactor
    /// (which then registers tables) and by unit tests.
    pub async fn flush_to_disk(&self) -> Result<Vec<FlushedTable>> {
        let drained: Vec<(Key, Vec<Value>)> = {
            let mut state = self.state.lock().await;
            std::mem::take(&mut state.buffers).into_iter().collect()
        };

        let mut flushed = Vec::new();
        let mut total_rows: u64 = 0;
        for ((tenant_id, table), rows) in drained {
            if rows.is_empty() {
                continue;
            }
            let row_count = rows.len();
            let path = match write_table_parquet(&self.warehouse_path, &tenant_id, &table, &rows) {
                Ok(p) => p,
                Err(e) => {
                    // Re-buffer so we don't lose data on transient IO errors.
                    warn!(error = %e, tenant = %tenant_id, table = %table, "flush failed; re-buffering rows");
                    let mut state = self.state.lock().await;
                    state
                        .buffers
                        .entry((tenant_id, table))
                        .or_default()
                        .extend(rows);
                    continue;
                }
            };
            total_rows += row_count as u64;
            flushed.push(FlushedTable {
                tenant_id: tenant_id.clone(),
                table_name: table.clone(),
                registered_as: registered_table_name(&tenant_id, &table),
                parquet_path: path,
                rows: row_count,
            });
        }

        let mut state = self.state.lock().await;
        state.last_flush = Some(Utc::now());
        state.total_rows_flushed += total_rows;
        state.total_files_flushed += flushed.len() as u64;
        Ok(flushed)
    }

    /// Drain the buffer to disk and register each freshly-written file with
    /// the engine. This is the path used by the background compactor.
    pub async fn flush(&self, handle: &EngineHandle) -> Result<Vec<FlushedTable>> {
        let flushed = self.flush_to_disk().await?;
        for t in &flushed {
            // `register_parquet` overwrites any existing registration — fine,
            // since we always want the engine pointed at the table directory
            // so it picks up every file that's been written so far.
            let dir = t
                .parquet_path
                .parent()
                .unwrap_or(&t.parquet_path)
                .to_string_lossy()
                .to_string();
            if let Err(e) = handle.register_parquet(&t.registered_as, &dir).await {
                warn!(
                    table = %t.registered_as,
                    error = %e,
                    "failed to register flushed table"
                );
            } else {
                info!(
                    table = %t.registered_as,
                    rows = t.rows,
                    path = %t.parquet_path.display(),
                    "ingest flush registered"
                );
            }
        }
        Ok(flushed)
    }
}

/// DataFusion table name that an ingest table is registered under. Tenant
/// prefix avoids collisions when multiple tenants ingest into a same-named
/// table inside the shared session.
pub fn registered_table_name(tenant_id: &str, table: &str) -> String {
    if tenant_id == opensnow_catalog::DEFAULT_TENANT {
        table.to_string()
    } else {
        format!("{tenant_id}__{table}")
    }
}

/// Spawn the background compaction task. The task wakes once a second; on
/// each tick it asks the buffer whether it's due for a flush.
pub fn spawn_compactor(buffer: SharedBuffer, handle: EngineHandle) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            ticker.tick().await;
            if buffer.should_flush().await {
                if let Err(e) = buffer.flush(&handle).await {
                    warn!(error = %e, "ingest compactor flush failed");
                } else {
                    debug!("ingest compactor flushed");
                }
            }
        }
    });
}

fn write_table_parquet(
    warehouse: &Path,
    tenant_id: &str,
    table: &str,
    rows: &[Value],
) -> Result<PathBuf> {
    if rows.is_empty() {
        anyhow::bail!("cannot flush an empty batch");
    }

    let schema =
        arrow::json::reader::infer_json_schema_from_iterator(rows.iter().map(|v| Ok(v.clone())))
            .context("failed to infer schema from batch")?;
    let schema: SchemaRef = Arc::new(schema);

    // arrow's JSON reader consumes newline-delimited JSON; feed each row as
    // its own line.
    let mut buf = Vec::with_capacity(rows.len() * 64);
    for r in rows {
        serde_json::to_writer(&mut buf, r)?;
        buf.push(b'\n');
    }
    let reader = ReaderBuilder::new(schema.clone())
        .build(std::io::Cursor::new(buf))
        .context("failed to build JSON reader")?;

    let mut batches: Vec<RecordBatch> = Vec::new();
    for batch in reader {
        batches.push(batch?);
    }
    if batches.is_empty() {
        anyhow::bail!("decoder produced no batches");
    }
    let combined = arrow::compute::concat_batches(&schema, &batches)?;

    let dir = warehouse.join(tenant_id).join("ingest").join(table);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let stamp = Utc::now().format("%Y%m%d_%H%M%S_%6f");
    let path = dir.join(format!("{stamp}.parquet"));

    let file =
        std::fs::File::create(&path).with_context(|| format!("creating {}", path.display()))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, combined.schema(), Some(props))?;
    writer.write(&combined)?;
    writer.close()?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_buf(threshold: usize, interval: Duration) -> (IngestBuffer, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let buffer = IngestBuffer::with_config(dir.path().to_path_buf(), threshold, interval);
        (buffer, dir)
    }

    #[tokio::test]
    async fn push_accumulates_rows() {
        let (buf, _dir) = make_buf(10_000, FLUSH_INTERVAL);
        buf.push(
            "default",
            "events",
            vec![json!({"id": 1}), json!({"id": 2})],
        )
        .await;
        buf.push("acme", "events", vec![json!({"id": 3})]).await;
        assert_eq!(buf.buffered_rows().await, 3);
    }

    #[tokio::test]
    async fn should_flush_when_threshold_hit() {
        // Tiny threshold so we don't have to push 10k rows.
        let (buf, _dir) = make_buf(5, Duration::from_secs(3600));
        for i in 0..5 {
            buf.push("default", "events", vec![json!({"id": i})]).await;
        }
        assert!(buf.should_flush().await, "threshold reached should fire");
    }

    #[tokio::test]
    async fn should_flush_when_interval_elapsed() {
        let (buf, _dir) = make_buf(1_000_000, Duration::from_millis(50));
        buf.push("default", "events", vec![json!({"id": 1})]).await;
        // No prior flush yet — first push past the interval triggers a flush.
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(buf.should_flush().await);
    }

    #[tokio::test]
    async fn empty_buffer_never_flushes() {
        let (buf, _dir) = make_buf(1, Duration::from_millis(1));
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!buf.should_flush().await);
    }

    #[tokio::test]
    async fn flush_writes_parquet_per_table_and_clears() {
        let (buf, dir) = make_buf(10_000, FLUSH_INTERVAL);
        buf.push(
            "default",
            "events",
            vec![json!({"id": 1, "name": "a"}), json!({"id": 2, "name": "b"})],
        )
        .await;
        buf.push("acme", "orders", vec![json!({"id": 1, "total": 10.5})])
            .await;

        let flushed = buf.flush_to_disk().await.unwrap();
        assert_eq!(flushed.len(), 2);
        // After flushing, buffer should be empty and stats updated.
        assert_eq!(buf.buffered_rows().await, 0);
        let status = buf.status().await;
        assert_eq!(status.total_rows_flushed, 3);
        assert_eq!(status.total_files_flushed, 2);
        assert!(status.last_flush.is_some());

        // Each flushed table should have a parquet file at the documented
        // path: <warehouse>/<tenant>/ingest/<table>/<stamp>.parquet.
        for t in &flushed {
            assert!(
                t.parquet_path.exists(),
                "parquet missing at {}",
                t.parquet_path.display()
            );
            assert!(
                t.parquet_path.starts_with(
                    dir.path()
                        .join(&t.tenant_id)
                        .join("ingest")
                        .join(&t.table_name)
                )
            );
        }
    }

    #[tokio::test]
    async fn registered_name_prefixes_non_default_tenant() {
        assert_eq!(registered_table_name("default", "events"), "events");
        assert_eq!(registered_table_name("acme", "events"), "acme__events");
    }

    #[tokio::test]
    async fn flushed_parquet_is_queryable() {
        // End-to-end check: write a batch, then read it back via DataFusion
        // directly so we know the file is well-formed.
        use datafusion::prelude::SessionContext;

        let (buf, dir) = make_buf(10_000, FLUSH_INTERVAL);
        buf.push(
            "default",
            "events",
            (0..50)
                .map(|i| json!({"id": i, "value": format!("row-{i}")}))
                .collect(),
        )
        .await;
        let flushed = buf.flush_to_disk().await.unwrap();
        assert_eq!(flushed.len(), 1);

        let ctx = SessionContext::new();
        let table_dir = dir.path().join("default").join("ingest").join("events");
        ctx.register_parquet("events", table_dir.to_str().unwrap(), Default::default())
            .await
            .unwrap();
        let batches = ctx
            .sql("SELECT count(*) AS c FROM events")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let arr = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        assert_eq!(arr.value(0), 50);
    }
}
