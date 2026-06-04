//! Pipe management -- Snowpipe-like abstraction for continuous file ingest.
//!
//! A **Pipe** defines a mapping from a source directory to a target table.
//! The `PipeManager` stores pipe definitions and can start / stop file
//! watchers for each pipe.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::config::{FileFormat, WatcherConfig};
use crate::file_watcher::FileWatcher;

/// A pipe definition that maps a source path to a warehouse table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipe {
    /// Unique name for this pipe.
    pub name: String,

    /// Directory to watch for incoming files.
    pub source_path: String,

    /// Target table name in the warehouse.
    pub target_table: String,

    /// Whether the pipe should automatically ingest new files.
    #[serde(default = "default_true")]
    pub auto_ingest: bool,

    /// Expected file format of incoming files.
    #[serde(default)]
    pub file_format: FileFormat,
}

fn default_true() -> bool {
    true
}

impl std::fmt::Display for Pipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Pipe(name={}, source={}, target={}, auto_ingest={}, format={})",
            self.name, self.source_path, self.target_table, self.auto_ingest, self.file_format
        )
    }
}

/// Manages pipe definitions and their associated file watchers.
pub struct PipeManager {
    warehouse_path: PathBuf,
    pipes: Arc<Mutex<HashMap<String, Pipe>>>,
    handles: Arc<Mutex<HashMap<String, JoinHandle<()>>>>,
}

impl PipeManager {
    /// Create a new `PipeManager`.
    pub fn new(warehouse_path: impl Into<PathBuf>) -> Self {
        Self {
            warehouse_path: warehouse_path.into(),
            pipes: Arc::new(Mutex::new(HashMap::new())),
            handles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// CREATE PIPE -- register a new pipe and optionally start its watcher.
    pub async fn create_pipe(&self, pipe: Pipe) -> anyhow::Result<()> {
        let name = pipe.name.clone();
        let auto = pipe.auto_ingest;

        {
            let mut pipes = self.pipes.lock().await;
            if pipes.contains_key(&name) {
                anyhow::bail!("Pipe '{}' already exists", name);
            }
            pipes.insert(name.clone(), pipe.clone());
        }

        info!(pipe = %name, "Pipe created");

        if auto {
            self.start_watcher(&pipe).await?;
        }

        Ok(())
    }

    /// DROP PIPE -- stop the watcher (if running) and remove the pipe.
    pub async fn drop_pipe(&self, name: &str) -> anyhow::Result<()> {
        self.stop_watcher(name).await;

        let mut pipes = self.pipes.lock().await;
        if pipes.remove(name).is_none() {
            anyhow::bail!("Pipe '{}' does not exist", name);
        }

        info!(pipe = %name, "Pipe dropped");
        Ok(())
    }

    /// SHOW PIPES -- return a snapshot of all registered pipes.
    pub async fn show_pipes(&self) -> Vec<Pipe> {
        let pipes = self.pipes.lock().await;
        let mut list: Vec<Pipe> = pipes.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list
    }

    /// Start a file watcher for the given pipe.
    async fn start_watcher(&self, pipe: &Pipe) -> anyhow::Result<()> {
        let config = WatcherConfig {
            watch_dir: pipe.source_path.clone(),
            warehouse_path: self.warehouse_path.to_string_lossy().to_string(),
            file_format: pipe.file_format,
        };

        let watcher = FileWatcher::from_config(&config);
        let name = pipe.name.clone();
        let name_for_log = name.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = watcher.run().await {
                warn!(pipe = %name_for_log, error = %e, "Pipe watcher exited with error");
            }
        });

        let mut handles = self.handles.lock().await;
        handles.insert(name.clone(), handle);

        info!(pipe = %name, "Watcher started");
        Ok(())
    }

    /// Stop a running watcher for the named pipe.
    async fn stop_watcher(&self, name: &str) {
        let mut handles = self.handles.lock().await;
        if let Some(handle) = handles.remove(name) {
            handle.abort();
            info!(pipe = %name, "Watcher stopped");
        }
    }

    /// Execute a SQL-like pipe command string.
    ///
    /// Supported commands:
    /// - `CREATE PIPE <name> SOURCE='<path>' TARGET='<table>' FORMAT=<parquet|csv|json> [AUTO_INGEST=true|false]`
    /// - `DROP PIPE <name>`
    /// - `SHOW PIPES`
    pub async fn execute_command(&self, sql: &str) -> anyhow::Result<String> {
        let upper = sql.trim().to_uppercase();

        if upper.starts_with("SHOW PIPES") {
            let pipes = self.show_pipes().await;
            if pipes.is_empty() {
                return Ok("No pipes defined.".to_string());
            }
            let mut out = String::new();
            for p in &pipes {
                out.push_str(&format!("{p}\n"));
            }
            return Ok(out);
        }

        if upper.starts_with("DROP PIPE") {
            let name = sql
                .trim()
                .strip_prefix("DROP PIPE")
                .or_else(|| sql.trim().strip_prefix("drop pipe"))
                .unwrap_or("")
                .trim()
                .trim_end_matches(';')
                .trim();
            if name.is_empty() {
                anyhow::bail!("Usage: DROP PIPE <name>");
            }
            self.drop_pipe(name).await?;
            return Ok(format!("Pipe '{name}' dropped."));
        }

        if upper.starts_with("CREATE PIPE") {
            let pipe = parse_create_pipe(sql)?;
            self.create_pipe(pipe).await?;
            return Ok("Pipe created.".to_string());
        }

        anyhow::bail!("Unknown pipe command: {sql}")
    }
}

/// Parse a `CREATE PIPE` statement.
///
/// Format:
/// ```text
/// CREATE PIPE my_pipe SOURCE='/watch/dir' TARGET='my_table' FORMAT=parquet AUTO_INGEST=true
/// ```
fn parse_create_pipe(sql: &str) -> anyhow::Result<Pipe> {
    let upper = sql.to_uppercase();

    // Extract pipe name -- the token right after "CREATE PIPE"
    let after_create = upper
        .strip_prefix("CREATE PIPE")
        .ok_or_else(|| anyhow::anyhow!("expected CREATE PIPE"))?
        .trim();
    let _upper_name = after_create
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing pipe name"))?;
    // Use original casing for the name.
    let name = sql.trim()["CREATE PIPE".len()..]
        .split_whitespace()
        .next()
        .unwrap()
        .trim_end_matches(';');

    let source = extract_quoted_param(sql, "SOURCE")
        .ok_or_else(|| anyhow::anyhow!("missing SOURCE='...'"))?;
    let target = extract_quoted_param(sql, "TARGET")
        .ok_or_else(|| anyhow::anyhow!("missing TARGET='...'"))?;

    let format = extract_param(&upper, "FORMAT").unwrap_or_else(|| "PARQUET".to_string());
    let file_format = match format.to_uppercase().as_str() {
        "PARQUET" => FileFormat::Parquet,
        "CSV" => FileFormat::Csv,
        "JSON" => FileFormat::Json,
        other => anyhow::bail!("unsupported format: {other}"),
    };

    let auto_ingest = extract_param(&upper, "AUTO_INGEST")
        .map(|v| v == "TRUE")
        .unwrap_or(true);

    Ok(Pipe {
        name: name.to_string(),
        source_path: source,
        target_table: target,
        auto_ingest,
        file_format,
    })
}

/// Extract a quoted parameter value like `KEY='value'`.
fn extract_quoted_param(sql: &str, key: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let pattern = format!("{key}='");
    let start = upper.find(&pattern)? + pattern.len();
    let rest = &sql[start..];
    let end = rest.find('\'')?;
    Some(rest[..end].to_string())
}

/// Extract a bare parameter value like `KEY=value`.
fn extract_param(upper_sql: &str, key: &str) -> Option<String> {
    let pattern = format!("{key}=");
    let start = upper_sql.find(&pattern)? + pattern.len();
    let rest = &upper_sql[start..];
    // Value ends at whitespace or end of string.
    let end = rest.find(' ').unwrap_or(rest.len());
    let val = rest[..end].trim_end_matches(';').to_string();
    if val.is_empty() { None } else { Some(val) }
}
