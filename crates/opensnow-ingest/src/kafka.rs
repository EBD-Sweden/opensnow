//! Kafka consumer that reads JSON messages and writes micro-batches as Parquet
//! to the warehouse.
//!
//! This module is only available when the `kafka` feature is enabled.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::json::ReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use rdkafka::ClientConfig;
use rdkafka::Message;
use rdkafka::consumer::{Consumer, StreamConsumer};
use tokio_stream::StreamExt;
use tracing::{debug, error, info, warn};

use crate::config::KafkaConfig;

/// A Kafka-to-Parquet ingester that consumes JSON messages, accumulates them
/// into micro-batches, and flushes each batch as a Parquet file.
pub struct KafkaIngester {
    brokers: String,
    topic: String,
    group_id: String,
    warehouse_path: PathBuf,
    table_name: String,
    batch_size: usize,
    flush_interval: Duration,
}

impl KafkaIngester {
    /// Create a new ingester with explicit parameters.
    pub fn new(
        brokers: impl Into<String>,
        topic: impl Into<String>,
        group_id: impl Into<String>,
        warehouse_path: impl Into<PathBuf>,
        table_name: impl Into<String>,
    ) -> Self {
        Self {
            brokers: brokers.into(),
            topic: topic.into(),
            group_id: group_id.into(),
            warehouse_path: warehouse_path.into(),
            table_name: table_name.into(),
            batch_size: 10_000,
            flush_interval: Duration::from_secs(60),
        }
    }

    /// Create an ingester from a [`KafkaConfig`].
    pub fn from_config(config: &KafkaConfig) -> Self {
        Self {
            brokers: config.brokers.clone(),
            topic: config.topic.clone(),
            group_id: config.group_id.clone(),
            warehouse_path: PathBuf::from(&config.warehouse_path),
            table_name: config.table_name.clone(),
            batch_size: config.batch_size,
            flush_interval: Duration::from_secs(config.flush_interval_secs),
        }
    }

    /// Override the default batch size (10 000 records).
    pub fn with_batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = batch_size;
        self
    }

    /// Override the default flush interval (60 s).
    pub fn with_flush_interval(mut self, interval: Duration) -> Self {
        self.flush_interval = interval;
        self
    }

    /// Target directory: `warehouse_path/opensnow/public/`
    fn table_dir(&self) -> PathBuf {
        self.warehouse_path.join("opensnow").join("public")
    }

    /// Run the consumer loop. This will block until cancelled.
    pub async fn run(&self) -> anyhow::Result<()> {
        let consumer: StreamConsumer = ClientConfig::new()
            .set("group.id", &self.group_id)
            .set("bootstrap.servers", &self.brokers)
            .set("auto.offset.reset", "earliest")
            .set("enable.auto.commit", "true")
            .create()?;

        consumer.subscribe(&[&self.topic])?;
        info!(
            topic = %self.topic,
            brokers = %self.brokers,
            batch_size = self.batch_size,
            flush_interval_secs = self.flush_interval.as_secs(),
            "Kafka ingester started"
        );

        let mut buf: Vec<Vec<u8>> = Vec::with_capacity(self.batch_size);
        let mut schema: Option<SchemaRef> = None;
        let mut last_flush = Instant::now();

        let mut stream = consumer.stream();

        loop {
            let timeout = self
                .flush_interval
                .checked_sub(last_flush.elapsed())
                .unwrap_or(Duration::ZERO);

            let msg = tokio::time::timeout(timeout, stream.next()).await;

            match msg {
                // Received a message before the timeout.
                Ok(Some(Ok(borrowed_msg))) => {
                    if let Some(payload) = borrowed_msg.payload() {
                        buf.push(payload.to_vec());

                        // Infer schema from the first message if we haven't yet.
                        if schema.is_none() {
                            match infer_schema_from_json(payload) {
                                Ok(s) => {
                                    info!("Inferred schema from first message: {:?}", s);
                                    schema = Some(Arc::new(s));
                                }
                                Err(e) => {
                                    warn!("Failed to infer schema from first message: {e}");
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    error!("Kafka consumer error: {e}");
                    continue;
                }
                // Stream ended (unlikely for Kafka).
                Ok(None) => {
                    info!("Kafka stream ended");
                    break;
                }
                // Timeout -- time to flush.
                Err(_) => {}
            }

            // Flush when we've accumulated enough records or the timer expired.
            let should_flush = buf.len() >= self.batch_size
                || (last_flush.elapsed() >= self.flush_interval && !buf.is_empty());

            if should_flush {
                if let Some(ref s) = schema {
                    if let Err(e) = self.flush_batch(&buf, s.clone()) {
                        error!("Failed to flush batch: {e}");
                    }
                } else {
                    warn!("No schema inferred yet; dropping {} records", buf.len());
                }
                buf.clear();
                last_flush = Instant::now();
            }
        }

        // Flush remaining records.
        if !buf.is_empty() {
            if let Some(ref s) = schema {
                self.flush_batch(&buf, s.clone())?;
            }
        }

        Ok(())
    }

    /// Convert a batch of JSON byte slices to a `RecordBatch` and write it as
    /// a Parquet file.
    fn flush_batch(&self, records: &[Vec<u8>], schema: SchemaRef) -> anyhow::Result<()> {
        debug!("Flushing {} records to Parquet", records.len());

        // Concatenate records as newline-delimited JSON.
        let mut combined = Vec::new();
        for rec in records {
            combined.extend_from_slice(rec);
            combined.push(b'\n');
        }

        let reader = ReaderBuilder::new(schema.clone()).build(std::io::Cursor::new(combined))?;

        let mut batches: Vec<RecordBatch> = Vec::new();
        for batch_result in reader {
            batches.push(batch_result?);
        }

        if batches.is_empty() {
            warn!(
                "No record batches produced from {} raw records",
                records.len()
            );
            return Ok(());
        }

        let batch = arrow::compute::concat_batches(&schema, &batches)?;
        let dir = self.table_dir();
        std::fs::create_dir_all(&dir)?;

        let file_name = format!(
            "{}_{}.parquet",
            self.table_name,
            chrono::Utc::now().format("%Y%m%d_%H%M%S_%3f")
        );
        let file_path = dir.join(&file_name);

        write_parquet(&file_path, &batch)?;

        info!(
            path = %file_path.display(),
            rows = batch.num_rows(),
            "Flushed Parquet partition"
        );
        Ok(())
    }
}

/// Infer an Arrow schema from a single JSON object.
fn infer_schema_from_json(payload: &[u8]) -> anyhow::Result<arrow::datatypes::Schema> {
    let value: serde_json::Value = serde_json::from_slice(payload)?;
    let schema = arrow::json::reader::infer_json_schema_from_iterator(std::iter::once(Ok(value)))?;
    Ok(schema)
}

/// Write a `RecordBatch` to a Parquet file with ZSTD compression.
fn write_parquet(path: &Path, batch: &RecordBatch) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .build();
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))?;
    writer.write(batch)?;
    writer.close()?;
    Ok(())
}
