use std::sync::Arc;

use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use serde::Deserialize;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum StorageConfig {
    Local {
        #[serde(default = "default_data_path")]
        path: String,
    },
    S3 {
        bucket: String,
        endpoint: Option<String>,
        region: Option<String>,
        access_key: Option<String>,
        secret_key: Option<String>,
    },
}

fn default_data_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    format!("{home}/.opensnow/data")
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self::Local {
            path: default_data_path(),
        }
    }
}

/// Create an ObjectStore instance from config.
pub fn create_object_store(config: &StorageConfig) -> anyhow::Result<Arc<dyn ObjectStore>> {
    match config {
        StorageConfig::Local { path } => {
            std::fs::create_dir_all(path)?;
            let store = LocalFileSystem::new_with_prefix(path)?;
            info!("Storage: local filesystem at {}", path);
            Ok(Arc::new(store))
        }
        StorageConfig::S3 {
            bucket,
            endpoint,
            region,
            access_key,
            secret_key,
        } => {
            let mut builder = object_store::aws::AmazonS3Builder::new().with_bucket_name(bucket);

            if let Some(endpoint) = endpoint {
                builder = builder.with_endpoint(endpoint).with_allow_http(true);
            }
            if let Some(region) = region {
                builder = builder.with_region(region);
            }
            if let Some(key) = access_key {
                builder = builder.with_access_key_id(key);
            }
            if let Some(secret) = secret_key {
                builder = builder.with_secret_access_key(secret);
            }

            let store = builder.build()?;
            info!(
                "Storage: S3 at {}/{}",
                endpoint.as_deref().unwrap_or("aws"),
                bucket
            );
            Ok(Arc::new(store))
        }
    }
}
