use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::Result;
use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use chrono::Utc;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::snapshot::Snapshot;

/// Iceberg-compatible table metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableMetadata {
    pub format_version: u32,
    pub table_uuid: String,
    pub location: String,
    pub schema: TableSchema,
    pub current_snapshot_id: Option<i64>,
    pub snapshots: Vec<Snapshot>,
    pub properties: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub schema_id: u32,
    pub fields: Vec<SchemaField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchemaField {
    pub id: u32,
    pub name: String,
    pub r#type: String,
    pub required: bool,
}

/// An Iceberg table that manages snapshots, data files, and metadata.
pub struct IcebergTable {
    metadata: TableMetadata,
    metadata_path: PathBuf,
    data_dir: PathBuf,
}

impl IcebergTable {
    /// Create a new Iceberg table at the given location.
    pub fn create(location: &str, schema: &SchemaRef) -> Result<Self> {
        let table_uuid = uuid::Uuid::new_v4().to_string();
        let data_dir = PathBuf::from(location).join("data");
        let metadata_dir = PathBuf::from(location).join("metadata");
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&metadata_dir)?;

        let fields: Vec<SchemaField> = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(i, f)| SchemaField {
                id: i as u32 + 1,
                name: f.name().clone(),
                r#type: format!("{:?}", f.data_type()),
                required: !f.is_nullable(),
            })
            .collect();

        let metadata = TableMetadata {
            format_version: 2,
            table_uuid: table_uuid.clone(),
            location: location.to_string(),
            schema: TableSchema {
                schema_id: 0,
                fields,
            },
            current_snapshot_id: None,
            snapshots: vec![],
            properties: std::collections::HashMap::new(),
        };

        let metadata_path = metadata_dir.join("v1.metadata.json");
        let json = serde_json::to_string_pretty(&metadata)?;
        std::fs::write(&metadata_path, &json)?;

        info!("Created Iceberg table at {}", location);

        Ok(Self {
            metadata,
            metadata_path,
            data_dir,
        })
    }

    /// Open an existing Iceberg table.
    pub fn open(location: &str) -> Result<Self> {
        let metadata_dir = PathBuf::from(location).join("metadata");
        let data_dir = PathBuf::from(location).join("data");

        // Find the latest metadata file
        let mut versions: Vec<_> = std::fs::read_dir(&metadata_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .collect();
        versions.sort_by_key(|e| e.path());

        let metadata_path = versions
            .last()
            .map(|e| e.path())
            .ok_or_else(|| anyhow::anyhow!("No metadata found at {}", location))?;

        let json = std::fs::read_to_string(&metadata_path)?;
        let metadata: TableMetadata = serde_json::from_str(&json)?;

        Ok(Self {
            metadata,
            metadata_path,
            data_dir,
        })
    }

    /// Append data as a new snapshot (atomic commit).
    pub fn append(&mut self, batches: &[RecordBatch]) -> Result<Snapshot> {
        if batches.is_empty() {
            return Err(anyhow::anyhow!("No data to append"));
        }

        let schema = batches[0].schema();
        // Use timestamp + atomic counter to ensure unique IDs even within the same millisecond
        static COUNTER: AtomicI64 = AtomicI64::new(0);
        let snapshot_id =
            Utc::now().timestamp_millis() * 1000 + COUNTER.fetch_add(1, Ordering::SeqCst);

        // Write data file
        let data_file = self.data_dir.join(format!("{snapshot_id}.parquet"));
        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build();
        let file = std::fs::File::create(&data_file)?;
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))?;

        let mut total_rows = 0u64;
        for batch in batches {
            total_rows += batch.num_rows() as u64;
            writer.write(batch)?;
        }
        writer.close()?;

        // Previous totals
        let prev_total_records = self
            .metadata
            .snapshots
            .last()
            .map(|s| s.summary.total_records)
            .unwrap_or(0);
        let prev_total_files = self
            .metadata
            .snapshots
            .last()
            .map(|s| s.summary.total_data_files)
            .unwrap_or(0);

        let snapshot = Snapshot::new_append(
            snapshot_id,
            self.metadata.current_snapshot_id,
            data_file.to_string_lossy().to_string(),
            1,
            total_rows,
            prev_total_records + total_rows,
            prev_total_files + 1,
        );

        // Atomic metadata update
        self.metadata.current_snapshot_id = Some(snapshot_id);
        self.metadata.snapshots.push(snapshot.clone());

        let version = self.metadata.snapshots.len();
        let new_metadata_path = self
            .metadata_path
            .parent()
            .unwrap()
            .join(format!("v{version}.metadata.json"));
        let json = serde_json::to_string_pretty(&self.metadata)?;
        std::fs::write(&new_metadata_path, &json)?;
        self.metadata_path = new_metadata_path;

        info!(
            "Appended {} rows as snapshot {} to {}",
            total_rows, snapshot_id, self.metadata.location
        );

        Ok(snapshot)
    }

    /// List all data files for the current snapshot.
    pub fn data_files(&self) -> Vec<String> {
        self.metadata
            .snapshots
            .iter()
            .map(|s| s.manifest_list.clone())
            .collect()
    }

    /// List all data files visible at a given timestamp (time travel).
    pub fn data_files_at(&self, timestamp_ms: i64) -> Vec<String> {
        self.metadata
            .snapshots
            .iter()
            .filter(|s| s.timestamp_ms <= timestamp_ms)
            .map(|s| s.manifest_list.clone())
            .collect()
    }

    /// Get snapshot by ID.
    pub fn snapshot(&self, snapshot_id: i64) -> Option<&Snapshot> {
        self.metadata
            .snapshots
            .iter()
            .find(|s| s.snapshot_id == snapshot_id)
    }

    /// List all snapshots (for SHOW SNAPSHOTS / time travel).
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.metadata.snapshots
    }

    pub fn current_snapshot_id(&self) -> Option<i64> {
        self.metadata.current_snapshot_id
    }

    pub fn location(&self) -> &str {
        &self.metadata.location
    }

    pub fn table_uuid(&self) -> &str {
        &self.metadata.table_uuid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};

    #[test]
    fn test_create_and_append() {
        let dir = tempfile::tempdir().unwrap();
        let location = dir.path().join("test_table");

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
        ]));

        let mut table = IcebergTable::create(location.to_str().unwrap(), &schema).unwrap();
        assert!(table.snapshots().is_empty());

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["a", "b", "c"])),
            ],
        )
        .unwrap();

        let snap = table.append(&[batch]).unwrap();
        assert_eq!(snap.summary.added_records, 3);
        assert_eq!(table.snapshots().len(), 1);
        assert_eq!(table.data_files().len(), 1);
    }

    #[test]
    fn test_multiple_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let location = dir.path().join("test_table2");

        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));

        let mut table = IcebergTable::create(location.to_str().unwrap(), &schema).unwrap();

        // Append twice with a small delay to ensure different timestamps
        let batch1 =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1, 2]))])
                .unwrap();
        table.append(&[batch1]).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));

        let batch2 = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int64Array::from(vec![3, 4, 5]))],
        )
        .unwrap();
        let snap2 = table.append(&[batch2]).unwrap();

        assert_eq!(table.snapshots().len(), 2);
        assert_eq!(snap2.summary.total_records, 5);
        assert_eq!(snap2.summary.total_data_files, 2);

        // All files should be visible at current time
        let all_files = table.data_files();
        assert_eq!(all_files.len(), 2);

        // Snapshot 1 should have a parent of None, snapshot 2 should have parent
        assert!(table.snapshots()[0].parent_snapshot_id.is_none());
        assert!(table.snapshots()[1].parent_snapshot_id.is_some());
    }

    #[test]
    fn test_open_existing() {
        let dir = tempfile::tempdir().unwrap();
        let location = dir.path().join("test_table3");

        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));

        let mut table = IcebergTable::create(location.to_str().unwrap(), &schema).unwrap();
        let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(Int64Array::from(vec![1]))])
            .unwrap();
        table.append(&[batch]).unwrap();

        // Re-open
        let table2 = IcebergTable::open(location.to_str().unwrap()).unwrap();
        assert_eq!(table2.snapshots().len(), 1);
    }
}
