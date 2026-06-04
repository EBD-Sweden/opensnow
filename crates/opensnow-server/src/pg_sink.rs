//! Postgres sink — run a query in OpenSnow and write the result set into an
//! external Postgres table (the "serving layer" pattern).
//!
//! Arrow result columns are cast to text for transport and re-typed in the
//! target table using a small, explicit Arrow→Postgres type map. String/JSON
//! values are escaped as SQL literals; numeric/boolean values are emitted raw.

use anyhow::{Context, Result, bail};
use arrow::array::{Array, ArrayRef, RecordBatch, StringArray};
use arrow::compute::cast;
use arrow::datatypes::DataType;
use opensnow_core::EngineHandle;

/// How to treat an existing target table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Drop and recreate the table from the query schema.
    Replace,
    /// Keep the table; append rows (table must already exist with matching cols).
    Append,
}

impl WriteMode {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "replace" | "" => Ok(Self::Replace),
            "append" => Ok(Self::Append),
            other => bail!("unknown mode '{other}' (expected 'replace' or 'append')"),
        }
    }
}

/// Map an Arrow column type to a Postgres column type.
fn pg_type(dt: &DataType) -> &'static str {
    match dt {
        DataType::Boolean => "boolean",
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => "bigint",
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => "bigint",
        DataType::Float16 | DataType::Float32 | DataType::Float64 => "double precision",
        DataType::Date32 | DataType::Date64 => "date",
        DataType::Timestamp(_, _) => "timestamp",
        _ => "text",
    }
}

/// Whether a Postgres column type needs its literal values single-quoted.
fn needs_quote(pg: &str) -> bool {
    matches!(pg, "text" | "date" | "timestamp")
}

fn is_safe_ident(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn sql_literal(value: &str, quote: bool) -> String {
    if quote {
        format!("'{}'", value.replace('\'', "''"))
    } else {
        value.to_string()
    }
}

/// Run `sql` in OpenSnow and load the result set into `schema.table` in the
/// Postgres database addressed by `dsn`. Returns the number of rows written.
pub async fn export_to_postgres(
    handle: &EngineHandle,
    sql: &str,
    dsn: &str,
    schema: &str,
    table: &str,
    mode: WriteMode,
) -> Result<usize> {
    if !is_safe_ident(schema) || !is_safe_ident(table) {
        bail!("invalid schema/table identifier");
    }
    let batches = handle
        .execute_sql(sql)
        .await
        .context("query failed in OpenSnow")?;
    let arrow_schema = match batches.first() {
        Some(b) => b.schema(),
        None => bail!("query returned no schema (no result batches)"),
    };
    let cols: Vec<(String, &'static str)> = arrow_schema
        .fields()
        .iter()
        .map(|f| (f.name().clone(), pg_type(f.data_type())))
        .collect();

    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect to target Postgres")?;
    // Drive the connection in the background for the duration of this call.
    let conn_task = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::error!("postgres connection error: {e}");
        }
    });

    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let result = load(&client, &cols, &batches, &qualified, schema, mode).await;
    drop(client); // closes the connection, lets conn_task finish
    let _ = conn_task.await;
    result
}

async fn load(
    client: &tokio_postgres::Client,
    cols: &[(String, &'static str)],
    batches: &[RecordBatch],
    qualified: &str,
    schema: &str,
    mode: WriteMode,
) -> Result<usize> {
    client
        .batch_execute(&format!(
            "CREATE SCHEMA IF NOT EXISTS {}",
            quote_ident(schema)
        ))
        .await
        .context("create schema")?;

    if mode == WriteMode::Replace {
        client
            .batch_execute(&format!("DROP TABLE IF EXISTS {qualified}"))
            .await
            .context("drop existing table")?;
        let coldefs = cols
            .iter()
            .map(|(name, ty)| format!("{} {ty}", quote_ident(name)))
            .collect::<Vec<_>>()
            .join(", ");
        client
            .batch_execute(&format!("CREATE TABLE {qualified} ({coldefs})"))
            .await
            .context("create target table")?;
    }

    let col_list = cols
        .iter()
        .map(|(n, _)| quote_ident(n))
        .collect::<Vec<_>>()
        .join(", ");

    let mut written = 0usize;
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        // Cast every column to Utf8 once; emit literals per the target PG type.
        let text_cols: Vec<ArrayRef> = batch
            .columns()
            .iter()
            .map(|c| cast(c, &DataType::Utf8))
            .collect::<std::result::Result<_, _>>()
            .context("cast result column to text")?;
        let str_cols: Vec<&StringArray> = text_cols
            .iter()
            .map(|a| {
                a.as_any()
                    .downcast_ref::<StringArray>()
                    .expect("cast to Utf8")
            })
            .collect();

        let mut tuples: Vec<String> = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let mut cells = Vec::with_capacity(cols.len());
            for (ci, (_, pg)) in cols.iter().enumerate() {
                let arr = str_cols[ci];
                if arr.is_null(row) {
                    cells.push("NULL".to_string());
                } else {
                    cells.push(sql_literal(arr.value(row), needs_quote(pg)));
                }
            }
            tuples.push(format!("({})", cells.join(", ")));
        }
        // Chunk multi-row INSERTs to keep statements a sane size.
        for chunk in tuples.chunks(500) {
            let stmt = format!(
                "INSERT INTO {qualified} ({col_list}) VALUES {}",
                chunk.join(", ")
            );
            client.batch_execute(&stmt).await.context("insert rows")?;
            written += chunk.len();
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_mapping_and_quoting() {
        assert_eq!(pg_type(&DataType::Int64), "bigint");
        assert_eq!(pg_type(&DataType::Float64), "double precision");
        assert_eq!(pg_type(&DataType::Utf8), "text");
        assert_eq!(pg_type(&DataType::Boolean), "boolean");
        assert!(needs_quote("text"));
        assert!(!needs_quote("bigint"));
    }

    #[test]
    fn literal_escaping_and_idents() {
        assert_eq!(sql_literal("O'Brien", true), "'O''Brien'");
        assert_eq!(sql_literal("42.5", false), "42.5");
        assert!(is_safe_ident("mart_gdp"));
        assert!(!is_safe_ident("1bad"));
        assert!(!is_safe_ident("drop;table"));
        assert_eq!(quote_ident("geo"), "\"geo\"");
    }

    #[test]
    fn write_mode_parsing() {
        assert_eq!(WriteMode::parse("replace").unwrap(), WriteMode::Replace);
        assert_eq!(WriteMode::parse("append").unwrap(), WriteMode::Append);
        assert!(WriteMode::parse("nonsense").is_err());
    }
}
