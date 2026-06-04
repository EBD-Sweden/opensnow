//! RAPIDS GPU acceleration backend for OpenSnow.
//!
//! Provides GPU-accelerated SQL execution (via cuDF) and vector search
//! (via cuVS/cuPy) with graceful CPU fallback.

use std::collections::HashMap;

pub mod python_bridge;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RapidsError {
    #[error("RAPIDS/cuDF not available on this system")]
    NotAvailable,
    #[error("Python bridge error: {0}")]
    BridgeError(String),
    #[error("Arrow IPC error: {0}")]
    IpcError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct RapidsConfig {
    pub enabled: bool,
    pub python_bin: String,
    pub gpu_memory_limit_mb: u64,
    pub fallback_to_cpu: bool,
}

impl Default for RapidsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            python_bin: "python3".to_string(),
            gpu_memory_limit_mb: 4096,
            fallback_to_cpu: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

pub struct RapidsBackend {
    config: RapidsConfig,
    available: bool,
}

impl RapidsBackend {
    /// Create a new backend, probing the system for RAPIDS/cuDF availability.
    pub fn new(config: RapidsConfig) -> Self {
        let available = check_availability(&config.python_bin);
        if available {
            tracing::info!("RAPIDS/cuDF detected — GPU acceleration enabled");
        } else {
            tracing::warn!("RAPIDS/cuDF not found — GPU acceleration disabled");
        }
        Self { config, available }
    }

    /// Whether the RAPIDS runtime was detected at construction time.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Borrow the current configuration.
    pub fn config(&self) -> &RapidsConfig {
        &self.config
    }

    /// Execute a SQL query against the supplied Arrow record-batches via the
    /// RAPIDS Python bridge.
    ///
    /// Each entry in `tables` maps a table name to a single `RecordBatch`.
    /// The bridge loads them into cuDF for GPU execution; if cuDF is absent,
    /// the Rust caller falls back to DataFusion automatically
    /// and returns the result set as Arrow IPC bytes which are decoded back
    /// into `RecordBatch`es.
    pub async fn execute_sql(
        &self,
        sql: &str,
        tables: HashMap<String, arrow::array::RecordBatch>,
    ) -> Result<Vec<arrow::array::RecordBatch>, RapidsError> {
        if !self.available {
            return Err(RapidsError::NotAvailable);
        }

        // Serialize every table to Arrow IPC and concatenate into a single
        // payload.  For simplicity the bridge currently accepts one table
        // encoded as IPC; extend as needed.
        let mut ipc_bytes: Vec<u8> = Vec::new();
        for batch in tables.values() {
            let mut writer = Vec::new();
            {
                let mut ipc_writer =
                    arrow::ipc::writer::StreamWriter::try_new(&mut writer, batch.schema_ref())
                        .map_err(|e| RapidsError::IpcError(e.to_string()))?;
                ipc_writer
                    .write(batch)
                    .map_err(|e| RapidsError::IpcError(e.to_string()))?;
                ipc_writer
                    .finish()
                    .map_err(|e| RapidsError::IpcError(e.to_string()))?;
            }
            ipc_bytes = writer;
        }

        let result_bytes = python_bridge::run_sql(&self.config.python_bin, sql, &ipc_bytes).await?;

        // Decode the result IPC stream back into RecordBatches.
        let cursor = std::io::Cursor::new(result_bytes);
        let reader = arrow::ipc::reader::StreamReader::try_new(cursor, None)
            .map_err(|e| RapidsError::IpcError(e.to_string()))?;

        let batches: Vec<arrow::array::RecordBatch> = reader
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| RapidsError::IpcError(e.to_string()))?;

        Ok(batches)
    }

    /// Perform a brute-force vector similarity search.
    ///
    /// Returns up to `top_k` results as `(index, score)` pairs sorted by
    /// descending similarity (cosine-like dot product).
    pub async fn vector_search(
        &self,
        embeddings: &[Vec<f32>],
        query: &[f32],
        top_k: usize,
    ) -> Result<Vec<(usize, f32)>, RapidsError> {
        if !self.available {
            return Err(RapidsError::NotAvailable);
        }

        python_bridge::run_vector_search(&self.config.python_bin, embeddings, query, top_k).await
    }
}

// ---------------------------------------------------------------------------
// Availability probe
// ---------------------------------------------------------------------------

/// Synchronously check whether `python_bin` can `import cudf`.
fn check_availability(python_bin: &str) -> bool {
    std::process::Command::new(python_bin)
        .args(["-c", "import cudf"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
