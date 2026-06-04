use serde::{Deserialize, Serialize};

/// Configuration for the ingest subsystem.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IngestConfig {
    /// Kafka-specific settings (only relevant when the `kafka` feature is enabled).
    #[serde(default)]
    pub kafka: KafkaConfig,

    /// File-watcher settings.
    #[serde(default)]
    pub watcher: WatcherConfig,
}

/// Kafka consumer configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KafkaConfig {
    /// Comma-separated broker list (e.g. "localhost:9092").
    #[serde(default = "default_brokers")]
    pub brokers: String,

    /// Consumer group id.
    #[serde(default = "default_group_id")]
    pub group_id: String,

    /// Topic to consume from.
    #[serde(default)]
    pub topic: String,

    /// Target table name in the warehouse.
    #[serde(default)]
    pub table_name: String,

    /// Path to the warehouse root directory.
    #[serde(default = "default_warehouse_path")]
    pub warehouse_path: String,

    /// Number of records to accumulate before flushing a batch.
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,

    /// Maximum seconds to wait before flushing a partial batch.
    #[serde(default = "default_flush_interval_secs")]
    pub flush_interval_secs: u64,
}

impl Default for KafkaConfig {
    fn default() -> Self {
        Self {
            brokers: default_brokers(),
            group_id: default_group_id(),
            topic: String::new(),
            table_name: String::new(),
            warehouse_path: default_warehouse_path(),
            batch_size: default_batch_size(),
            flush_interval_secs: default_flush_interval_secs(),
        }
    }
}

/// File-watcher configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Directory to watch for incoming files.
    #[serde(default)]
    pub watch_dir: String,

    /// Warehouse root path where files are ingested to.
    #[serde(default = "default_warehouse_path")]
    pub warehouse_path: String,

    /// Expected file format of incoming files.
    #[serde(default)]
    pub file_format: FileFormat,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            watch_dir: String::new(),
            warehouse_path: default_warehouse_path(),
            file_format: FileFormat::Parquet,
        }
    }
}

/// Supported file formats for auto-ingest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    #[default]
    Parquet,
    Csv,
    Json,
}

impl FileFormat {
    /// Return the file extension (without the dot).
    pub fn extension(&self) -> &'static str {
        match self {
            FileFormat::Parquet => "parquet",
            FileFormat::Csv => "csv",
            FileFormat::Json => "json",
        }
    }

    /// Try to detect format from a file path extension.
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| match ext.to_lowercase().as_str() {
                "parquet" => Some(FileFormat::Parquet),
                "csv" => Some(FileFormat::Csv),
                "json" | "jsonl" | "ndjson" => Some(FileFormat::Json),
                _ => None,
            })
    }
}

impl std::fmt::Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileFormat::Parquet => write!(f, "parquet"),
            FileFormat::Csv => write!(f, "csv"),
            FileFormat::Json => write!(f, "json"),
        }
    }
}

fn default_brokers() -> String {
    "localhost:9092".to_string()
}

fn default_group_id() -> String {
    "opensnow-ingest".to_string()
}

fn default_warehouse_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/.opensnow/data")
}

fn default_batch_size() -> usize {
    10_000
}

fn default_flush_interval_secs() -> u64 {
    60
}
