use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use arrow::datatypes::SchemaRef;
use tokio::sync::RwLock;
use tracing::info;

use crate::table::IcebergTable;

/// Iceberg catalog that manages table locations and metadata.
/// For local mode, tables are stored under warehouse_path/database/schema/table_name/.
pub struct IcebergCatalog {
    warehouse_path: PathBuf,
    tables: Arc<RwLock<HashMap<String, String>>>, // full_name -> location
}

impl IcebergCatalog {
    pub fn new(warehouse_path: &str) -> Self {
        std::fs::create_dir_all(warehouse_path).ok();
        Self {
            warehouse_path: PathBuf::from(warehouse_path),
            tables: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Create a new Iceberg table.
    pub async fn create_table(
        &self,
        database: &str,
        schema_name: &str,
        table_name: &str,
        arrow_schema: &SchemaRef,
    ) -> Result<IcebergTable> {
        let location = self
            .warehouse_path
            .join(database)
            .join(schema_name)
            .join(table_name);

        let table = IcebergTable::create(location.to_str().unwrap(), arrow_schema)?;

        let full_name = format!("{database}.{schema_name}.{table_name}");
        let mut tables = self.tables.write().await;
        tables.insert(full_name.clone(), location.to_string_lossy().to_string());

        info!("Created Iceberg table: {}", full_name);
        Ok(table)
    }

    /// Open an existing Iceberg table.
    pub async fn open_table(
        &self,
        database: &str,
        schema_name: &str,
        table_name: &str,
    ) -> Result<IcebergTable> {
        let location = self
            .warehouse_path
            .join(database)
            .join(schema_name)
            .join(table_name);

        IcebergTable::open(location.to_str().unwrap())
    }

    /// List all tables in the catalog.
    pub async fn list_tables(&self) -> Vec<String> {
        let tables = self.tables.read().await;
        tables.keys().cloned().collect()
    }

    /// Scan the warehouse directory for existing Iceberg tables.
    pub async fn scan_warehouse(&self) -> Result<Vec<String>> {
        let mut found = Vec::new();

        // Walk warehouse_path/*/schema/table/metadata/
        if let Ok(databases) = std::fs::read_dir(&self.warehouse_path) {
            for db_entry in databases.flatten() {
                if !db_entry.file_type()?.is_dir() {
                    continue;
                }
                let db_name = db_entry.file_name().to_string_lossy().to_string();

                if let Ok(schemas) = std::fs::read_dir(db_entry.path()) {
                    for schema_entry in schemas.flatten() {
                        if !schema_entry.file_type()?.is_dir() {
                            continue;
                        }
                        let schema_name = schema_entry.file_name().to_string_lossy().to_string();

                        if let Ok(tables) = std::fs::read_dir(schema_entry.path()) {
                            for table_entry in tables.flatten() {
                                if !table_entry.file_type()?.is_dir() {
                                    continue;
                                }
                                let table_name =
                                    table_entry.file_name().to_string_lossy().to_string();

                                // Check if it has a metadata directory (Iceberg table)
                                let metadata_dir = table_entry.path().join("metadata");
                                if metadata_dir.exists() {
                                    let full_name = format!("{db_name}.{schema_name}.{table_name}");
                                    let location = table_entry.path().to_string_lossy().to_string();

                                    let mut tables = self.tables.write().await;
                                    tables.insert(full_name.clone(), location);
                                    found.push(full_name);
                                }
                            }
                        }
                    }
                }
            }
        }

        info!("Scanned warehouse: found {} Iceberg tables", found.len());
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{DataType, Field, Schema};

    #[tokio::test]
    async fn test_create_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = IcebergCatalog::new(dir.path().to_str().unwrap());

        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));

        catalog
            .create_table("mydb", "public", "test_table", &schema)
            .await
            .unwrap();

        let tables = catalog.list_tables().await;
        assert_eq!(tables.len(), 1);
        assert!(tables[0].contains("test_table"));
    }

    #[tokio::test]
    async fn test_scan_warehouse() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = IcebergCatalog::new(dir.path().to_str().unwrap());

        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));

        catalog
            .create_table("db1", "public", "t1", &schema)
            .await
            .unwrap();
        catalog
            .create_table("db1", "public", "t2", &schema)
            .await
            .unwrap();

        // New catalog instance scanning the same directory
        let catalog2 = IcebergCatalog::new(dir.path().to_str().unwrap());
        let found = catalog2.scan_warehouse().await.unwrap();
        assert_eq!(found.len(), 2);
    }
}
