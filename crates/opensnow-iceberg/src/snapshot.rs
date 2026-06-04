use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// An Iceberg-compatible snapshot representing a point-in-time state of a table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub snapshot_id: i64,
    pub parent_snapshot_id: Option<i64>,
    pub timestamp_ms: i64,
    pub operation: SnapshotOperation,
    pub manifest_list: String,
    pub summary: SnapshotSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SnapshotOperation {
    #[serde(rename = "append")]
    Append,
    #[serde(rename = "overwrite")]
    Overwrite,
    #[serde(rename = "delete")]
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotSummary {
    pub added_data_files: u64,
    pub deleted_data_files: u64,
    pub added_records: u64,
    pub deleted_records: u64,
    pub total_records: u64,
    pub total_data_files: u64,
}

impl Snapshot {
    pub fn new_append(
        snapshot_id: i64,
        parent: Option<i64>,
        manifest_list: String,
        added_files: u64,
        added_records: u64,
        total_records: u64,
        total_files: u64,
    ) -> Self {
        Self {
            snapshot_id,
            parent_snapshot_id: parent,
            timestamp_ms: Utc::now().timestamp_millis(),
            operation: SnapshotOperation::Append,
            manifest_list,
            summary: SnapshotSummary {
                added_data_files: added_files,
                deleted_data_files: 0,
                added_records,
                deleted_records: 0,
                total_records,
                total_data_files: total_files,
            },
        }
    }

    pub fn timestamp(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.timestamp_ms).unwrap_or_default()
    }
}
