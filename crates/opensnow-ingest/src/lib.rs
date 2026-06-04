//! opensnow-ingest -- streaming ingestion for the OpenSnow warehouse.
//!
//! This crate provides:
//!
//! - **Kafka consumer** (`kafka` feature) that reads JSON messages and writes
//!   micro-batched Parquet files to the warehouse.
//! - **File watcher** for Snowpipe-like auto-ingest of Parquet, CSV, and JSON
//!   files from a watched directory.
//! - **Pipe management** for defining, starting, and stopping ingest pipes via
//!   SQL-like commands.

pub mod config;
pub mod file_watcher;
pub mod pipe;

#[cfg(feature = "kafka")]
pub mod kafka;

pub use config::{FileFormat, IngestConfig};
pub use file_watcher::FileWatcher;
pub use pipe::{Pipe, PipeManager};
