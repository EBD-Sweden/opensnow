use anyhow::Result;
use arrow::array::RecordBatch;
use chrono::{DateTime, NaiveDateTime, Utc};
use datafusion::prelude::*;
use tracing::info;

use crate::table::IcebergTable;

/// Execute a time-travel query against an Iceberg table.
/// Reads only data files that were visible at the given timestamp.
pub async fn query_at_timestamp(
    table: &IcebergTable,
    timestamp: &str,
    sql_after_from: &str,
) -> Result<Vec<RecordBatch>> {
    // Parse timestamp
    let ts = parse_timestamp(timestamp)?;
    let ts_ms = ts.timestamp_millis();

    // Get data files visible at that timestamp
    let files = table.data_files_at(ts_ms);
    if files.is_empty() {
        return Ok(vec![]);
    }

    info!(
        "Time travel to {}: {} data files visible",
        timestamp,
        files.len()
    );

    // Create a temporary DataFusion context and register all visible files
    let ctx = SessionContext::new();
    for (i, file) in files.iter().enumerate() {
        let alias = format!("__tt_part_{i}");
        ctx.register_parquet(&alias, file, Default::default())
            .await?;
    }

    // UNION ALL all partitions
    if files.len() == 1 {
        let df = ctx
            .sql(&format!("SELECT * FROM __tt_part_0 {sql_after_from}"))
            .await?;
        Ok(df.collect().await?)
    } else {
        let unions: Vec<String> = (0..files.len())
            .map(|i| format!("SELECT * FROM __tt_part_{i}"))
            .collect();
        let union_sql = unions.join(" UNION ALL ");
        let full_sql = format!("SELECT * FROM ({union_sql}) {sql_after_from}");
        let df = ctx.sql(&full_sql).await?;
        Ok(df.collect().await?)
    }
}

fn parse_timestamp(s: &str) -> Result<DateTime<Utc>> {
    // Try various formats
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt.and_utc());
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt.and_utc());
    }
    // Date only
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let dt = d.and_hms_opt(23, 59, 59).unwrap();
        return Ok(dt.and_utc());
    }
    Err(anyhow::anyhow!(
        "Cannot parse timestamp: '{}'. Use YYYY-MM-DD or YYYY-MM-DD HH:MM:SS",
        s
    ))
}

/// List snapshots for a table (SHOW SNAPSHOTS command).
pub fn list_snapshots(table: &IcebergTable) -> Vec<SnapshotInfo> {
    table
        .snapshots()
        .iter()
        .map(|s| SnapshotInfo {
            snapshot_id: s.snapshot_id,
            timestamp: s.timestamp().to_rfc3339(),
            operation: format!("{:?}", s.operation),
            added_records: s.summary.added_records,
            total_records: s.summary.total_records,
            total_files: s.summary.total_data_files,
        })
        .collect()
}

#[derive(Debug)]
pub struct SnapshotInfo {
    pub snapshot_id: i64,
    pub timestamp: String,
    pub operation: String,
    pub added_records: u64,
    pub total_records: u64,
    pub total_files: u64,
}
