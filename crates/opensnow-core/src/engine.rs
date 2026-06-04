use std::sync::Arc;

use anyhow::Context;
use arrow::array::RecordBatch;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::prelude::*;
use object_store::ObjectStore;
use object_store::aws::AmazonS3Builder;
use object_store::azure::MicrosoftAzureBuilder;
use object_store::gcp::GoogleCloudStorageBuilder;
use tracing::info;
use url::Url;

use opensnow_catalog::{Catalog, QueryRecordInput};

use crate::error::Result;

#[cfg(feature = "rapids")]
use opensnow_rapids::{RapidsBackend, RapidsConfig};

// ─── Storage performance tuning constants ───────────────────────────────────
//
// These values are tuned for cloud object storage (S3/GCS/Azure Blob).
// The key insight vs local disk:
//   - Object storage has high latency per request (~1-10ms)
//   - But very high bandwidth per connection (multiple GB/s)
//   - Strategy: fewer, larger, parallel requests
//
// Reference: DataFusion Parquet execution config docs
// https://docs.rs/datafusion/latest/datafusion/config/struct.ParquetOptions.html

/// Max parallel row group readers per partition.
/// Each row group is an independent S3/GCS/Azure range GET.
/// 8 = saturate cloud storage bandwidth without excessive connection overhead.
#[allow(dead_code)]
const CLOUD_PARALLEL_ROW_GROUP_READERS: &str = "8";

/// Request coalescing: merge S3 range requests within this byte window.
/// DataFusion splits Parquet column chunks into separate requests by default.
/// Coalescing merges nearby requests into one — fewer round trips, same data.
/// 16MB = good balance; turbolite uses similar logic with 256-page groups.
#[allow(dead_code)]
const CLOUD_REQUEST_COALESCE_BYTES: &str = "16777216"; // 16 MB

/// Prefetch size for each Parquet row group fetch.
/// When reading a row group, prefetch this many bytes ahead.
/// 64MB = typical row group size; fetches entire row group in one request.
#[allow(dead_code)]
const CLOUD_PREFETCH_BYTES: &str = "67108864"; // 64 MB

// ─── Engine configuration ────────────────────────────────────────────────────

/// Configuration for the OpenSnow engine.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub warehouse_path: String,

    // ── AWS S3 / MinIO / S3-compatible ──
    pub s3_endpoint: Option<String>, // None = real AWS S3; Some = MinIO/R2/etc.
    pub s3_bucket: Option<String>,
    pub s3_region: Option<String>,
    pub s3_allow_insecure_http: bool, // true only for private local MinIO HTTP endpoints
    pub s3_access_key: Option<String>, // None = IRSA / instance profile
    pub s3_secret_key: Option<String>,

    // ── Google Cloud Storage ──
    pub gcs_bucket: Option<String>,
    pub gcs_service_account_path: Option<String>, // None = Workload Identity / ADC
    pub gcs_project_id: Option<String>,

    // ── Azure Blob Storage ──
    pub azure_container: Option<String>,
    pub azure_account_name: Option<String>,
    pub azure_account_key: Option<String>, // None = Managed Identity
    pub azure_client_id: Option<String>,   // for service principal auth
    pub azure_client_secret: Option<String>,
    pub azure_tenant_id: Option<String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Self {
            warehouse_path: format!("{home}/.opensnow/warehouse"),
            s3_endpoint: None,
            s3_bucket: None,
            s3_region: None,
            s3_allow_insecure_http: false,
            s3_access_key: None,
            s3_secret_key: None,
            gcs_bucket: None,
            gcs_service_account_path: None,
            gcs_project_id: None,
            azure_container: None,
            azure_account_name: None,
            azure_account_key: None,
            azure_client_id: None,
            azure_client_secret: None,
            azure_tenant_id: None,
        }
    }
}

/// The core query engine wrapping Apache DataFusion.
pub struct OpenSnowEngine {
    ctx: SessionContext,
    config: EngineConfig,
    catalog: Catalog,
    #[cfg(feature = "rapids")]
    rapids: Option<Arc<opensnow_rapids::RapidsBackend>>,
}

impl OpenSnowEngine {
    pub fn new() -> Self {
        Self::with_config(EngineConfig::default())
    }

    /// Build engine with config and an explicit catalog path.
    /// Preferred constructor when `OpenSnowConfig` is available.
    pub fn from_config_and_catalog(config: EngineConfig, catalog_path: &str) -> Self {
        Self::try_from_config_and_catalog(config, catalog_path).unwrap_or_else(|e| {
            panic!("Failed to initialize OpenSnow engine: {e:#}");
        })
    }

    /// Fallible constructor for service startup paths that need actionable
    /// storage/catalog errors instead of partial initialization or silent warns.
    pub fn try_from_config_and_catalog(
        config: EngineConfig,
        catalog_path: &str,
    ) -> anyhow::Result<Self> {
        Self::build(config, catalog_path)
    }

    /// Legacy constructor — uses default catalog path derived from HOME.
    pub fn with_config(config: EngineConfig) -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let default_catalog = format!("{home}/.opensnow/catalog.db");
        Self::from_config_and_catalog(config, &default_catalog)
    }

    fn build(config: EngineConfig, catalog_path: &str) -> anyhow::Result<Self> {
        // ── DataFusion session config ─────────────────────────────────────────
        // Cloud storage tuning: parallel reads, request coalescing, prefetch.
        // These are no-ops for local filesystem (zero overhead when not on cloud).
        let session_config = SessionConfig::new()
            .with_information_schema(true)
            .with_default_catalog_and_schema("opensnow", "public")
            .with_batch_size(8192)
            // Push filters down into Parquet page/row-group level — skips entire
            // row groups without reading them from S3 (the #1 perf win for OLAP).
            .set_str("datafusion.execution.parquet.pushdown_filters", "true")
            // Reorder filters: apply cheaper filters (low-selectivity) first to
            // maximise row group skipping before expensive ones.
            .set_str("datafusion.execution.parquet.reorder_filters", "true");

        let runtime = Arc::new(RuntimeEnv::default());
        let ctx = SessionContext::new_with_config_rt(session_config, runtime);

        // ── Register cloud object stores ──────────────────────────────────────
        if let Some(ref bucket) = config.s3_bucket.clone()
            && let Err(e) = Self::register_s3(&ctx, &config, bucket)
        {
            tracing::warn!("Failed to register S3 object store: {}", e);
        }
        if let Some(ref bucket) = config.gcs_bucket.clone()
            && let Err(e) = Self::register_gcs(&ctx, &config, bucket)
        {
            tracing::warn!("Failed to register GCS object store: {}", e);
        }
        if let Some(ref container) = config.azure_container.clone()
            && let Err(e) = Self::register_azure(&ctx, &config, container)
        {
            tracing::warn!("Failed to register Azure Blob object store: {}", e);
        }

        // Ensure warehouse directory exists for local storage before opening the
        // catalog. Failing fast here avoids creating/upgrading catalog state for
        // a demo instance that cannot write table data.
        let warehouse_path = std::path::Path::new(&config.warehouse_path);
        if warehouse_path.exists() && !warehouse_path.is_dir() {
            anyhow::bail!(
                "warehouse path exists but is not a directory: {}; set [storage].warehouse_path to a writable directory or move the file before startup",
                warehouse_path.display()
            );
        }
        std::fs::create_dir_all(warehouse_path).with_context(|| {
            format!(
                "failed to create warehouse directory {}; check volume permissions or configure object storage/local persistence before startup",
                warehouse_path.display()
            )
        })?;

        // Open the catalog (SQLite)
        let catalog = Catalog::open(catalog_path)
            .with_context(|| format!("Failed to open catalog at {catalog_path}"))?;

        info!("OpenSnow engine initialized (DataFusion)");
        info!("Warehouse path: {}", config.warehouse_path);
        info!("Catalog: {}", catalog_path);
        info!(
            "Cloud storage: S3={} GCS={} Azure={}",
            config.s3_bucket.is_some(),
            config.gcs_bucket.is_some(),
            config.azure_container.is_some(),
        );

        Ok(Self {
            ctx,
            config,
            catalog,
            #[cfg(feature = "rapids")]
            rapids: None,
        })
    }

    // ── AWS S3 / MinIO / S3-compatible ───────────────────────────────────────

    fn register_s3(
        ctx: &SessionContext,
        config: &EngineConfig,
        bucket: &str,
    ) -> anyhow::Result<()> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            // Use virtual-hosted-style URLs (required for AWS S3, optional for MinIO)
            .with_virtual_hosted_style_request(config.s3_endpoint.is_none());

        if let Some(ref endpoint) = config.s3_endpoint {
            // MinIO / R2 / custom S3-compatible endpoint
            if endpoint.starts_with("http://") && !config.s3_allow_insecure_http {
                anyhow::bail!(
                    "refusing insecure HTTP S3 endpoint; set s3_allow_insecure_http=true only for private MinIO demos"
                );
            }
            builder = builder.with_endpoint(endpoint);
            if config.s3_allow_insecure_http {
                builder = builder.with_allow_http(true);
            }
        }
        if let Some(ref region) = config.s3_region {
            builder = builder.with_region(region);
        }
        // Explicit credentials. If omitted, falls back to:
        //   AWS: IRSA → instance profile → env vars → ~/.aws/credentials
        if let Some(ref key) = config.s3_access_key {
            builder = builder.with_access_key_id(key);
        }
        if let Some(ref secret) = config.s3_secret_key {
            builder = builder.with_secret_access_key(secret);
        }

        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        let url = Url::parse(&format!("s3://{bucket}"))?;
        ctx.register_object_store(&url, store);
        info!(
            "Registered S3 object store: s3://{} (VPC endpoint routes this privately on AWS)",
            bucket
        );
        Ok(())
    }

    /// Register an additional S3 bucket at runtime (e.g. external tables).
    pub fn register_s3_bucket(
        &self,
        bucket: &str,
        endpoint: Option<&str>,
        region: Option<&str>,
        access_key: Option<&str>,
        secret_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut builder = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_virtual_hosted_style_request(endpoint.is_none());
        if let Some(ep) = endpoint {
            builder = builder.with_endpoint(ep).with_allow_http(true);
        }
        if let Some(r) = region {
            builder = builder.with_region(r);
        }
        if let Some(k) = access_key {
            builder = builder.with_access_key_id(k);
        }
        if let Some(s) = secret_key {
            builder = builder.with_secret_access_key(s);
        }
        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        let url = Url::parse(&format!("s3://{bucket}"))?;
        self.ctx.register_object_store(&url, store);
        info!("Registered S3 bucket at runtime: s3://{}", bucket);
        Ok(())
    }

    // ── Google Cloud Storage ──────────────────────────────────────────────────

    fn register_gcs(
        ctx: &SessionContext,
        config: &EngineConfig,
        bucket: &str,
    ) -> anyhow::Result<()> {
        let mut builder = GoogleCloudStorageBuilder::new().with_bucket_name(bucket);

        if let Some(ref sa_path) = config.gcs_service_account_path {
            // Explicit service account JSON key file
            builder = builder.with_service_account_path(sa_path);
        }
        // If no service account path is provided, the GCS client will use the
        // default credential chain:
        //   - GKE: Workload Identity (recommended — zero credentials in config)
        //   - Local: Application Default Credentials (gcloud auth login)
        // NOTE: `gcs_project_id` is currently unused because `object_store`
        // does not expose a project-level configuration option on the
        // `GoogleCloudStorageBuilder` as of 0.11.
        // TODO: when `object_store` exposes project configuration, thread
        // `config.gcs_project_id` through here.

        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        let url = Url::parse(&format!("gs://{bucket}"))?;
        ctx.register_object_store(&url, store);
        info!(
            "Registered GCS object store: gs://{} (Private Google Access active on GKE)",
            bucket
        );
        Ok(())
    }

    /// Register an additional GCS bucket at runtime.
    pub fn register_gcs_bucket(
        &self,
        bucket: &str,
        service_account_path: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut builder = GoogleCloudStorageBuilder::new().with_bucket_name(bucket);
        if let Some(sa) = service_account_path {
            builder = builder.with_service_account_path(sa);
        }
        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        let url = Url::parse(&format!("gs://{bucket}"))?;
        self.ctx.register_object_store(&url, store);
        info!("Registered GCS bucket at runtime: gs://{}", bucket);
        Ok(())
    }

    // ── Azure Blob Storage ────────────────────────────────────────────────────

    fn register_azure(
        ctx: &SessionContext,
        config: &EngineConfig,
        container: &str,
    ) -> anyhow::Result<()> {
        let mut builder = MicrosoftAzureBuilder::new().with_container_name(container);

        if let Some(ref account) = config.azure_account_name {
            builder = builder.with_account(account);
        }
        if let Some(ref key) = config.azure_account_key {
            // Storage account key (simple, for dev/non-prod)
            builder = builder.with_access_key(key);
        } else if let (Some(client_id), Some(secret), Some(tenant)) = (
            config.azure_client_id.clone(),
            config.azure_client_secret.clone(),
            config.azure_tenant_id.clone(),
        ) {
            // Service principal (recommended for prod)
            builder = builder
                .with_client_id(client_id)
                .with_client_secret(secret)
                .with_tenant_id(tenant);
        }
        // If no credentials: falls back to:
        //   AKS: Managed Identity / Workload Identity (recommended — zero credentials)
        //   Local: Azure CLI credentials (az login)

        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        // Azure uses: abfs://container@account.dfs.core.windows.net/
        let account_name = config.azure_account_name.as_deref().unwrap_or("default");
        let url = Url::parse(&format!(
            "abfs://{}@{}.dfs.core.windows.net/",
            container, account_name
        ))?;
        ctx.register_object_store(&url, store);
        info!(
            "Registered Azure Blob object store: abfs://{}@{} (Private Endpoint on AKS)",
            container, account_name
        );
        Ok(())
    }

    /// Register an additional Azure container at runtime.
    pub fn register_azure_container(
        &self,
        container: &str,
        account_name: &str,
        account_key: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut builder = MicrosoftAzureBuilder::new()
            .with_container_name(container)
            .with_account(account_name);
        if let Some(key) = account_key {
            builder = builder.with_access_key(key);
        }
        let store: Arc<dyn ObjectStore> = Arc::new(builder.build()?);
        let url = Url::parse(&format!(
            "abfs://{}@{}.dfs.core.windows.net/",
            container, account_name
        ))?;
        self.ctx.register_object_store(&url, store);
        info!(
            "Registered Azure container at runtime: abfs://{}@{}",
            container, account_name
        );
        Ok(())
    }

    /// Execute a SQL query and return Arrow RecordBatches.
    /// Custom commands (COPY INTO, SHOW TABLES, etc.) are intercepted first.
    pub async fn execute_sql(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        // Check for custom commands first
        if let Some(batches) = crate::commands::handle_command(self, sql).await? {
            return Ok(batches);
        }
        self.execute_sql_raw(sql).await
    }

    /// Execute SQL directly via DataFusion (no custom command interception).
    pub async fn execute_sql_raw(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let df = self.ctx.sql(sql).await?;
        let batches = df.collect().await?;
        Ok(batches)
    }

    /// Execute SQL and return a DataFrame.
    pub async fn sql(&self, sql: &str) -> Result<DataFrame> {
        let df = self.ctx.sql(sql).await?;
        Ok(df)
    }

    /// Re-register all known materialized views as Parquet tables.
    ///
    /// Called on engine startup so that materialized views persisted in the
    /// catalog are queryable as ordinary tables in the new session.
    pub async fn register_materialized_views(&self) -> Result<()> {
        let views = self
            .catalog
            .list_materialized_views()
            .map_err(|e| crate::error::OpenSnowError::Internal(format!("catalog error: {e}")))?;
        for mv in &views {
            if !std::path::Path::new(&mv.parquet_path).exists() {
                tracing::warn!(
                    "Materialized view '{}' parquet missing at {} — skipping registration",
                    mv.name,
                    mv.parquet_path
                );
                continue;
            }
            if let Err(e) = self
                .ctx
                .register_parquet(&mv.name, &mv.parquet_path, Default::default())
                .await
            {
                tracing::warn!("Failed to register materialized view '{}': {}", mv.name, e);
            } else {
                info!(
                    "Registered materialized view: {} -> {}",
                    mv.name, mv.parquet_path
                );
            }
        }
        Ok(())
    }

    /// Register a Parquet file/directory as a named table.
    pub async fn register_parquet(&self, name: &str, path: &str) -> Result<()> {
        self.ctx
            .register_parquet(name, path, Default::default())
            .await?;
        info!("Registered Parquet table: {} -> {}", name, path);
        Ok(())
    }

    /// Register a CSV file as a named table.
    pub async fn register_csv(&self, name: &str, path: &str) -> Result<()> {
        self.ctx
            .register_csv(name, path, Default::default())
            .await?;
        info!("Registered CSV table: {} -> {}", name, path);
        Ok(())
    }

    #[cfg(feature = "rapids")]
    pub fn with_rapids(config: EngineConfig, rapids_config: opensnow_rapids::RapidsConfig) -> Self {
        let mut engine = Self::with_config(config);
        let backend = opensnow_rapids::RapidsBackend::new(rapids_config);
        if backend.is_available() {
            tracing::info!("RAPIDS GPU backend available — GPU acceleration enabled");
        } else {
            tracing::warn!("RAPIDS GPU backend not available — falling back to CPU");
        }
        engine.rapids = Some(std::sync::Arc::new(backend));
        engine
    }

    /// Execute SQL with GPU acceleration if RAPIDS is available, otherwise falls back to CPU DataFusion.
    #[cfg(feature = "rapids")]
    pub async fn execute_sql_gpu(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        if let Some(ref rapids) = self.rapids {
            if rapids.is_available() {
                tracing::debug!("Routing query to GPU (RAPIDS/cuDF)");
                // For now, pass empty tables map — full table registration to be wired in Phase 2
                match rapids
                    .execute_sql(sql, std::collections::HashMap::new())
                    .await
                {
                    Ok(batches) => return Ok(batches),
                    Err(opensnow_rapids::RapidsError::NotAvailable) => {
                        tracing::warn!("GPU path unavailable, falling back to CPU");
                    }
                    Err(e) => {
                        tracing::warn!("GPU query failed ({}), falling back to CPU", e);
                    }
                }
            }
        }
        // CPU fallback
        self.execute_sql(sql).await
    }

    #[cfg(feature = "rapids")]
    pub fn rapids_backend(&self) -> Option<&opensnow_rapids::RapidsBackend> {
        self.rapids.as_deref()
    }

    /// Access the catalog for warehouse/table metadata queries.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Record a query in the catalog query history table (default tenant).
    ///
    /// This is best-effort only: failures are logged but do not affect
    /// user-visible query execution.
    pub fn record_query_history(
        &self,
        warehouse: &str,
        sql: &str,
        duration_ms: i64,
        rows_returned: i64,
        rows_scanned: Option<i64>,
        status: &str,
    ) {
        self.record_query_history_for_tenant(
            opensnow_catalog::DEFAULT_TENANT,
            warehouse,
            sql,
            duration_ms,
            rows_returned,
            rows_scanned,
            status,
        );
    }

    /// Record a query in the catalog query history table, scoped to a tenant.
    #[allow(clippy::too_many_arguments)]
    pub fn record_query_history_for_tenant(
        &self,
        tenant_id: &str,
        warehouse: &str,
        sql: &str,
        duration_ms: i64,
        rows_returned: i64,
        rows_scanned: Option<i64>,
        status: &str,
    ) {
        let record = QueryRecordInput {
            user_name: None,
            warehouse: warehouse.to_string(),
            sql: sql.to_string(),
            duration_ms,
            rows_returned,
            rows_scanned,
            status: status.to_string(),
        };

        if let Err(e) = self
            .catalog
            .insert_query_record_for_tenant(tenant_id, &record)
        {
            tracing::warn!("failed to record query history: {}", e);
        }
    }

    pub fn session_context(&self) -> &SessionContext {
        &self.ctx
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub fn warehouse_path(&self) -> &str {
        &self.config.warehouse_path
    }

    /// Build the on-disk warehouse directory for a tenant. Each tenant lives
    /// in its own sub-directory under the configured `warehouse_path` so that
    /// per-tenant Parquet files cannot collide.
    pub fn tenant_warehouse_path(&self, tenant_id: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.config.warehouse_path).join(tenant_id)
    }
}

impl Default for OpenSnowEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn isolated_test_engine() -> (tempfile::TempDir, OpenSnowEngine) {
        let dir = tempfile::tempdir().unwrap();
        let config = EngineConfig {
            warehouse_path: dir.path().join("warehouse").to_string_lossy().into_owned(),
            ..Default::default()
        };
        let catalog = dir.path().join("catalog/catalog.db");
        let engine =
            OpenSnowEngine::try_from_config_and_catalog(config, catalog.to_str().unwrap()).unwrap();

        (dir, engine)
    }

    #[test]
    fn test_try_engine_init_creates_local_warehouse_directory_and_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let warehouse = dir.path().join("warehouse");
        let catalog = dir.path().join("catalog/catalog.db");
        let config = EngineConfig {
            warehouse_path: warehouse.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let engine =
            OpenSnowEngine::try_from_config_and_catalog(config, catalog.to_str().unwrap()).unwrap();

        assert!(warehouse.is_dir());
        assert!(catalog.exists());
        assert_eq!(engine.warehouse_path(), warehouse.to_str().unwrap());
    }

    #[test]
    fn test_try_engine_init_rejects_warehouse_path_that_is_file() {
        let dir = tempfile::tempdir().unwrap();
        let warehouse = dir.path().join("warehouse-file");
        let mut file = std::fs::File::create(&warehouse).unwrap();
        writeln!(file, "not a directory").unwrap();
        let catalog = dir.path().join("catalog.db");
        let config = EngineConfig {
            warehouse_path: warehouse.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let err =
            match OpenSnowEngine::try_from_config_and_catalog(config, catalog.to_str().unwrap()) {
                Ok(_) => panic!("expected warehouse file path to fail storage preflight"),
                Err(err) => err.to_string(),
            };

        assert!(
            err.contains("warehouse path exists but is not a directory"),
            "{err}"
        );
        assert!(
            !catalog.exists(),
            "catalog should not be created when storage preflight fails"
        );
    }

    #[tokio::test]
    async fn test_basic_query() {
        let (_dir, engine) = isolated_test_engine();
        let batches = engine
            .execute_sql("SELECT 1 AS num, 'opensnow' AS name")
            .await
            .unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn test_aggregation() {
        let (_dir, engine) = isolated_test_engine();
        let sql = "SELECT COUNT(*) AS cnt FROM (VALUES (1), (2), (3)) AS t(x)";
        let batches = engine.execute_sql(sql).await.unwrap();
        assert_eq!(batches[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn test_register_and_query_parquet() {
        use arrow::array::{Int64Array, StringArray};
        use arrow::datatypes::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;
        use std::fs::File;

        let (_engine_dir, engine) = isolated_test_engine();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["alice", "bob", "charlie"])),
            ],
        )
        .unwrap();

        let file = File::create(&path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        // Register and query via table name
        engine
            .register_parquet("test_table", path.to_str().unwrap())
            .await
            .unwrap();
        let result = engine
            .execute_sql("SELECT * FROM test_table ORDER BY id")
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 3);

        // Test aggregation on parquet
        let result = engine
            .execute_sql("SELECT COUNT(*) AS cnt FROM test_table")
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn test_information_schema() {
        let (_dir, engine) = isolated_test_engine();
        let batches = engine
            .execute_sql("SELECT * FROM information_schema.tables")
            .await
            .unwrap();
        assert!(!batches.is_empty());
    }
}
