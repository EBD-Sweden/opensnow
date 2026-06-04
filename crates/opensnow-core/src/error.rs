use thiserror::Error;

#[derive(Error, Debug)]
pub enum OpenSnowError {
    #[error("Query execution error: {0}")]
    QueryExecution(#[from] datafusion::error::DataFusionError),

    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    #[error("Storage error: {0}")]
    Storage(#[from] object_store::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, OpenSnowError>;
