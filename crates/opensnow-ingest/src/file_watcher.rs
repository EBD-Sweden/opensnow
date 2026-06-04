//! File watcher for Snowpipe-like auto-ingest.
//!
//! Monitors a directory for new Parquet, CSV, or JSON files and copies them
//! into the warehouse.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::config::{FileFormat, WatcherConfig};

/// Watches a directory for new files and ingests them into the warehouse.
pub struct FileWatcher {
    watch_dir: PathBuf,
    warehouse_path: PathBuf,
    file_format: FileFormat,
}

impl FileWatcher {
    /// Create a new `FileWatcher`.
    pub fn new(
        watch_dir: impl Into<PathBuf>,
        warehouse_path: impl Into<PathBuf>,
        file_format: FileFormat,
    ) -> Self {
        Self {
            watch_dir: watch_dir.into(),
            warehouse_path: warehouse_path.into(),
            file_format,
        }
    }

    /// Create a `FileWatcher` from configuration.
    pub fn from_config(config: &WatcherConfig) -> Self {
        Self {
            watch_dir: PathBuf::from(&config.watch_dir),
            warehouse_path: PathBuf::from(&config.warehouse_path),
            file_format: config.file_format,
        }
    }

    /// Target directory inside the warehouse.
    fn target_dir(&self) -> PathBuf {
        self.warehouse_path.join("opensnow").join("public")
    }

    /// Start watching. This runs until the returned handle is dropped or an
    /// error occurs.
    pub async fn run(&self) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::channel::<PathBuf>(256);

        let expected_ext = self.file_format.extension().to_string();

        let sender = tx.clone();
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
                        for path in event.paths {
                            let matches_ext = path
                                .extension()
                                .and_then(|e| e.to_str())
                                .is_some_and(|e| e == expected_ext);
                            if !matches_ext {
                                continue;
                            }
                            if let Err(e) = sender.blocking_send(path) {
                                error!("Failed to send file event: {e}");
                            }
                        }
                    }
                }
                Err(e) => error!("Watch error: {e}"),
            },
            notify::Config::default(),
        )?;

        std::fs::create_dir_all(&self.watch_dir)?;
        watcher.watch(&self.watch_dir, RecursiveMode::NonRecursive)?;

        info!(
            dir = %self.watch_dir.display(),
            format = %self.file_format,
            "File watcher started"
        );

        let target_dir = self.target_dir();
        std::fs::create_dir_all(&target_dir)?;

        // Keep the watcher alive by holding it in an Arc.
        let _watcher = Arc::new(watcher);

        while let Some(src_path) = rx.recv().await {
            info!(path = %src_path.display(), "New file detected");

            if let Err(e) = ingest_file(&src_path, &target_dir) {
                error!(
                    path = %src_path.display(),
                    error = %e,
                    "Failed to ingest file"
                );
            }
        }

        Ok(())
    }
}

/// Copy a file into the warehouse target directory.
fn ingest_file(src: &Path, target_dir: &Path) -> anyhow::Result<()> {
    let file_name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("source path has no file name"))?;

    let dest = target_dir.join(file_name);

    if dest.exists() {
        // Generate a unique name to avoid collisions.
        let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("");
        let unique = format!(
            "{}_{}.{}",
            stem,
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f"),
            ext
        );
        let dest = target_dir.join(unique);
        std::fs::copy(src, &dest)?;
        info!(dest = %dest.display(), "Ingested file (renamed to avoid collision)");
    } else {
        std::fs::copy(src, &dest)?;
        info!(dest = %dest.display(), "Ingested file");
    }

    Ok(())
}

/// List all files in the target directory that match the given format.
pub fn list_ingested_files(
    warehouse_path: &Path,
    format: FileFormat,
) -> anyhow::Result<Vec<PathBuf>> {
    let dir = warehouse_path.join("opensnow").join("public");
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let ext = format.extension();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some(ext) {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}
