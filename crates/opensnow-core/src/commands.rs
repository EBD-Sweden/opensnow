use std::path::Path;
use std::sync::Arc;

use arrow::array::{Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{DataType, Field, Schema};

use tracing::info;

use crate::engine::OpenSnowEngine;
use crate::error::Result;

/// Handle custom SQL commands that DataFusion doesn't natively support.
/// Returns Some(batches) if the command was handled, None if it should
/// be passed through to DataFusion.
pub async fn handle_command(
    engine: &OpenSnowEngine,
    sql: &str,
) -> Result<Option<Vec<RecordBatch>>> {
    let trimmed = sql.trim();
    let upper = trimmed.to_uppercase();

    // ── Virtual warehouse commands ─────────────────────────────────────────
    if upper.starts_with("SHOW WAREHOUSES") {
        return handle_show_warehouses(engine).await.map(Some);
    }
    if upper.starts_with("CREATE WAREHOUSE") {
        return handle_create_warehouse(engine, trimmed).await.map(Some);
    }
    if upper.starts_with("ALTER WAREHOUSE") {
        return handle_alter_warehouse(engine, trimmed).await.map(Some);
    }
    if upper.starts_with("USE WAREHOUSE") {
        return handle_use_warehouse(engine, trimmed).await.map(Some);
    }

    if upper.starts_with("COPY INTO") || upper.starts_with("COPY ") {
        return handle_copy_into(engine, trimmed).await.map(Some);
    }

    if keyword_sequence_end(trimmed, &["CREATE", "TABLE"]).is_some_and(|table_pos| {
        !contains_keyword(trimmed, "EXTERNAL") && find_as_clause(trimmed, table_pos).is_some()
    }) {
        return handle_ctas(engine, trimmed).await.map(Some);
    }

    // ── Materialized views ─────────────────────────────────────────────────
    if keyword_sequence_end(trimmed, &["CREATE", "MATERIALIZED", "VIEW"]).is_some() {
        return handle_create_materialized_view(engine, trimmed)
            .await
            .map(Some);
    }
    if upper.starts_with("REFRESH MATERIALIZED VIEW") {
        return handle_refresh_materialized_view(engine, trimmed)
            .await
            .map(Some);
    }
    if upper.starts_with("DROP MATERIALIZED VIEW") {
        return handle_drop_materialized_view(engine, trimmed)
            .await
            .map(Some);
    }

    if upper.starts_with("SHOW TABLES") {
        return handle_show_tables(engine).await.map(Some);
    }

    if upper.starts_with("SHOW DATABASES") || upper.starts_with("SHOW SCHEMAS") {
        return handle_show_databases(engine).await.map(Some);
    }

    if upper.starts_with("DESCRIBE ") || upper.starts_with("DESC ") {
        let table = trimmed.split_whitespace().nth(1).unwrap_or("");
        return handle_describe(engine, table).await.map(Some);
    }

    if upper.starts_with("DROP TABLE ") {
        let table = trimmed.split_whitespace().nth(2).unwrap_or("");
        return handle_drop_table(engine, table).await.map(Some);
    }

    Ok(None)
}

fn is_safe_unqualified_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn validate_output_table_identifier(name: &str, command: &str) -> Result<()> {
    if is_safe_unqualified_identifier(name) {
        Ok(())
    } else {
        Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid {command} target identifier '{name}'. Use an unquoted table name with letters, numbers, and underscores only"
        )))
    }
}

fn validate_warehouse_identifier(name: &str, command: &str) -> Result<()> {
    if is_safe_unqualified_identifier(name) {
        Ok(())
    } else {
        Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid {command} target identifier '{name}'. Use an unquoted warehouse name with letters, numbers, and underscores only"
        )))
    }
}

fn normalize_warehouse_size(size: &str) -> Result<String> {
    let normalized = size.to_ascii_lowercase();
    match normalized.as_str() {
        "xsmall" | "small" | "medium" | "large" | "xlarge" => Ok(normalized),
        _ => Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid CREATE WAREHOUSE SIZE '{size}'. Use one of xsmall, small, medium, large, xlarge"
        ))),
    }
}

fn parse_warehouse_i64(value: &str, option: &str) -> Result<i64> {
    value.parse::<i64>().map_err(|_| {
        crate::error::OpenSnowError::Internal(format!(
            "Invalid CREATE WAREHOUSE {option} value '{value}'. Expected an integer"
        ))
    })
}

fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                if chars.peek() == Some(&q) {
                    current.push(chars.next().unwrap());
                } else {
                    quote = None;
                }
            }
            continue;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                current.push(ch);
            }
            ';' => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    statements.push(trimmed.to_string());
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }

    let trimmed = current.trim();
    if !trimmed.is_empty() {
        statements.push(trimmed.to_string());
    }
    statements
}

fn first_keyword(sql: &str) -> String {
    sql.trim_start()
        .split(|c: char| c.is_whitespace() || c == '(')
        .find(|token| !token.is_empty())
        .unwrap_or_default()
        .to_ascii_uppercase()
}

fn contains_keyword(sql: &str, expected: &str) -> bool {
    sql.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|token| token.eq_ignore_ascii_case(expected))
}

fn is_sql_identifier_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn find_as_clause(sql: &str, start: usize) -> Option<(usize, usize)> {
    let mut quote: Option<char> = None;
    let mut chars = sql.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
        if idx < start {
            continue;
        }

        if let Some(q) = quote {
            if ch == q {
                if chars.peek().is_some_and(|(_, next)| *next == q) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            'A' | 'a' => {
                let after_a = idx + ch.len_utf8();
                let Some(s_char) = sql[after_a..].chars().next() else {
                    continue;
                };
                if !s_char.eq_ignore_ascii_case(&'s') {
                    continue;
                }
                let as_end = after_a + s_char.len_utf8();
                let before_is_boundary = sql[..idx]
                    .chars()
                    .next_back()
                    .is_none_or(|prev| !is_sql_identifier_char(prev));
                let after_is_boundary = sql[as_end..]
                    .chars()
                    .next()
                    .is_none_or(|next| !is_sql_identifier_char(next));
                if before_is_boundary && after_is_boundary {
                    let query_start = as_end
                        + sql[as_end..]
                            .chars()
                            .take_while(|c| c.is_whitespace())
                            .map(char::len_utf8)
                            .sum::<usize>();
                    return Some((idx, query_start));
                }
            }
            _ => {}
        }
    }

    None
}

fn keyword_sequence_end(sql: &str, expected: &[&str]) -> Option<usize> {
    let mut offset = 0;
    for expected_token in expected {
        let leading_whitespace = sql[offset..]
            .chars()
            .take_while(|c| c.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>();
        let token_start = offset + leading_whitespace;
        let rest = &sql[token_start..];
        let token_len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let token = &rest[..token_len];
        if !token.eq_ignore_ascii_case(expected_token) {
            return None;
        }
        offset = token_start + token_len;
    }
    Some(offset)
}

fn validate_materialization_query(query_sql: &str, command: &str) -> Result<String> {
    let statements = split_sql_statements(query_sql);
    if statements.len() != 1 {
        return Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid {command} materialization query: expected exactly one SELECT or WITH statement"
        )));
    }

    let statement = statements.into_iter().next().unwrap();
    match first_keyword(&statement).as_str() {
        "SELECT" | "WITH" => Ok(statement),
        keyword => Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid {command} materialization query: expected SELECT or WITH, got {keyword}"
        ))),
    }
}

// ── Virtual warehouse SQL handlers ───────────────────────────────────────

async fn handle_show_warehouses(engine: &OpenSnowEngine) -> Result<Vec<RecordBatch>> {
    let warehouses = engine.catalog().list_warehouses().map_err(|e| {
        crate::error::OpenSnowError::Internal(format!("Failed to list warehouses: {e}"))
    })?;

    let names: Vec<&str> = warehouses.iter().map(|w| w.name.as_str()).collect();
    let sizes: Vec<&str> = warehouses.iter().map(|w| w.size.as_str()).collect();
    let states: Vec<&str> = warehouses.iter().map(|w| w.state.as_str()).collect();
    let min_nodes: Vec<i64> = warehouses.iter().map(|w| w.min_nodes).collect();
    let max_nodes: Vec<i64> = warehouses.iter().map(|w| w.max_nodes).collect();
    let auto_suspend: Vec<i64> = warehouses.iter().map(|w| w.auto_suspend_seconds).collect();

    let schema = Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("size", DataType::Utf8, false),
        Field::new("state", DataType::Utf8, false),
        Field::new("min_nodes", DataType::Int64, false),
        Field::new("max_nodes", DataType::Int64, false),
        Field::new("auto_suspend_seconds", DataType::Int64, false),
    ]));

    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(names)),
            Arc::new(StringArray::from(sizes)),
            Arc::new(StringArray::from(states)),
            Arc::new(Int64Array::from(min_nodes)),
            Arc::new(Int64Array::from(max_nodes)),
            Arc::new(Int64Array::from(auto_suspend)),
        ],
    )?;

    Ok(vec![batch])
}

async fn handle_create_warehouse(engine: &OpenSnowEngine, sql: &str) -> Result<Vec<RecordBatch>> {
    // Syntax: CREATE WAREHOUSE <name> [WITH SIZE = '<size>' MIN_NODES = <n> MAX_NODES = <n> AUTO_SUSPEND = <seconds>]
    let upper = sql.to_uppercase();
    let after_kw = sql["CREATE WAREHOUSE".len()..]
        .trim()
        .trim_end_matches(';')
        .trim();

    // Extract and validate warehouse name (first token) before catalog mutation.
    let name = after_kw.split_whitespace().next().ok_or_else(|| {
        crate::error::OpenSnowError::Internal("CREATE WAREHOUSE requires a name".into())
    })?;
    validate_warehouse_identifier(name, "CREATE WAREHOUSE")?;

    // Parse optional WITH clause. These values are catalog metadata in the
    // current local/single-node slice, so fail closed rather than storing values
    // that would later imply unsupported resource/cost semantics.
    let mut size = "small".to_string();
    let mut min_nodes: i64 = 0;
    let mut max_nodes: i64 = 4;
    let mut auto_suspend: i64 = 300;

    if upper.contains("WITH") {
        let tokens: Vec<String> = after_kw
            .split(|c: char| c.is_whitespace() || c == '=' || c == ',' || c == ';')
            .filter(|t| !t.is_empty())
            .map(|t| t.trim_matches('\'').trim_matches('"').to_string())
            .collect();
        let mut i = 1; // token 0 is the already-validated warehouse name
        while i < tokens.len() {
            match tokens[i].to_ascii_uppercase().as_str() {
                "WITH" => i += 1,
                "SIZE" => {
                    let value = tokens.get(i + 1).ok_or_else(|| {
                        crate::error::OpenSnowError::Internal(
                            "Invalid CREATE WAREHOUSE SIZE: missing value".into(),
                        )
                    })?;
                    size = normalize_warehouse_size(value)?;
                    i += 2;
                }
                "MIN_NODES" => {
                    let value = tokens.get(i + 1).ok_or_else(|| {
                        crate::error::OpenSnowError::Internal(
                            "Invalid CREATE WAREHOUSE MIN_NODES: missing value".into(),
                        )
                    })?;
                    min_nodes = parse_warehouse_i64(value, "MIN_NODES")?;
                    i += 2;
                }
                "MAX_NODES" => {
                    let value = tokens.get(i + 1).ok_or_else(|| {
                        crate::error::OpenSnowError::Internal(
                            "Invalid CREATE WAREHOUSE MAX_NODES: missing value".into(),
                        )
                    })?;
                    max_nodes = parse_warehouse_i64(value, "MAX_NODES")?;
                    i += 2;
                }
                "AUTO_SUSPEND" | "AUTO_SUSPEND_SECONDS" => {
                    let value = tokens.get(i + 1).ok_or_else(|| {
                        crate::error::OpenSnowError::Internal(
                            "Invalid CREATE WAREHOUSE AUTO_SUSPEND: missing value".into(),
                        )
                    })?;
                    auto_suspend = parse_warehouse_i64(value, "AUTO_SUSPEND")?;
                    i += 2;
                }
                option => {
                    return Err(crate::error::OpenSnowError::Internal(format!(
                        "Invalid CREATE WAREHOUSE option '{option}'. Supported options: SIZE, MIN_NODES, MAX_NODES, AUTO_SUSPEND"
                    )));
                }
            }
        }
    }

    if min_nodes < 0 {
        return Err(crate::error::OpenSnowError::Internal(
            "Invalid CREATE WAREHOUSE MIN_NODES: value must be non-negative".into(),
        ));
    }
    if max_nodes < 0 {
        return Err(crate::error::OpenSnowError::Internal(
            "Invalid CREATE WAREHOUSE MAX_NODES: value must be non-negative".into(),
        ));
    }
    if max_nodes < min_nodes {
        return Err(crate::error::OpenSnowError::Internal(
            "Invalid CREATE WAREHOUSE MAX_NODES: value must be greater than or equal to MIN_NODES"
                .into(),
        ));
    }
    if auto_suspend < 0 {
        return Err(crate::error::OpenSnowError::Internal(
            "Invalid CREATE WAREHOUSE AUTO_SUSPEND: value must be non-negative".into(),
        ));
    }

    engine
        .catalog()
        .create_warehouse(name, &size, min_nodes, max_nodes, auto_suspend)
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!("Failed to create warehouse: {e}"))
        })?;

    let schema = Arc::new(Schema::new(vec![Field::new(
        "status",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(vec![format!(
            "WAREHOUSE CREATED: {name}"
        )]))],
    )?;
    Ok(vec![batch])
}

async fn handle_alter_warehouse(engine: &OpenSnowEngine, sql: &str) -> Result<Vec<RecordBatch>> {
    // Syntax: ALTER WAREHOUSE <name> RESUME|SUSPEND|SET STATE = RUNNING|SUSPENDED
    let after_kw = &sql["ALTER WAREHOUSE".len()..].trim();
    let upper_rest = after_kw.to_uppercase();

    let name = after_kw.split_whitespace().next().ok_or_else(|| {
        crate::error::OpenSnowError::Internal("ALTER WAREHOUSE requires a name".into())
    })?;

    let mut parts = upper_rest.split_whitespace();
    let _name = parts.next();
    let first_action = parts.next().unwrap_or_default();
    let state = if first_action == "RESUME" {
        "RUNNING".to_string()
    } else if first_action == "SUSPEND" {
        "SUSPENDED".to_string()
    } else {
        let set_pos = upper_rest.find("SET").ok_or_else(|| {
            crate::error::OpenSnowError::Internal(
                "ALTER WAREHOUSE requires RESUME, SUSPEND, or SET STATE = RUNNING|SUSPENDED".into(),
            )
        })?;
        let after_set = &upper_rest[set_pos + 3..].trim().to_string();

        if !after_set.starts_with("STATE") {
            return Err(crate::error::OpenSnowError::Internal(
                "ALTER WAREHOUSE currently supports RESUME, SUSPEND, or SET STATE = RUNNING|SUSPENDED".into(),
            ));
        }

        let eq_pos = after_set.find('=').ok_or_else(|| {
            crate::error::OpenSnowError::Internal("Expected '=' after STATE".into())
        })?;
        after_set[eq_pos + 1..]
            .split_whitespace()
            .next()
            .unwrap_or("SUSPENDED")
            .to_string()
    };

    if state != "RUNNING" && state != "SUSPENDED" {
        return Err(crate::error::OpenSnowError::Internal(format!(
            "Invalid state '{}'. Use RUNNING or SUSPENDED",
            state
        )));
    }

    engine
        .catalog()
        .update_warehouse_state(name, &state)
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!("Failed to alter warehouse: {e}"))
        })?;

    // TODO(Phase 2): Update Prometheus metrics via control-plane.
    // Cannot call opensnow-server::metrics from core to avoid circular deps.
    // When operator/control-plane watches catalog, it will set:
    //   set_warehouse_status(name, "state", if RUNNING 1.0 else 0.0)

    info!("ALTER WAREHOUSE {} SET STATE = {}", name, state);

    let schema = Arc::new(Schema::new(vec![Field::new(
        "status",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(vec![format!(
            "WAREHOUSE {name} STATE SET TO {state}"
        )]))],
    )?;
    Ok(vec![batch])
}

async fn handle_use_warehouse(engine: &OpenSnowEngine, sql: &str) -> Result<Vec<RecordBatch>> {
    // Syntax: USE WAREHOUSE <name>
    let after_kw = &sql["USE WAREHOUSE".len()..].trim();
    let name = after_kw.split_whitespace().next().ok_or_else(|| {
        crate::error::OpenSnowError::Internal("USE WAREHOUSE requires a name".into())
    })?;

    // Validate warehouse exists
    let wh = engine.catalog().get_warehouse(name).map_err(|e| {
        crate::error::OpenSnowError::Internal(format!("Failed to look up warehouse: {e}"))
    })?;

    if wh.is_none() {
        return Err(crate::error::OpenSnowError::Internal(format!(
            "Warehouse '{}' does not exist",
            name
        )));
    }

    // Phase 1: no-op marker — real per-session routing comes in Phase 2
    info!("USE WAREHOUSE {} (Phase 1: no-op marker)", name);

    let schema = Arc::new(Schema::new(vec![Field::new(
        "status",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(vec![format!(
            "WAREHOUSE SET: {name}"
        )]))],
    )?;
    Ok(vec![batch])
}

// ── Data loading / DDL handlers ─────────────────────────────────────────

async fn handle_copy_into(engine: &OpenSnowEngine, sql: &str) -> Result<Vec<RecordBatch>> {
    // Parse: COPY INTO <table> FROM '<path>' [FILE_FORMAT = (TYPE = PARQUET|CSV)]
    // Simple parser — handles: COPY INTO table FROM 'path'
    let parts: Vec<&str> = sql.splitn(5, char::is_whitespace).collect();
    if parts.len() < 4 {
        return Err(crate::error::OpenSnowError::Internal(
            "Invalid COPY INTO syntax. Use: COPY INTO <table> FROM '<path>'".into(),
        ));
    }

    // Find table name and source path
    let upper = sql.to_uppercase();
    let into_pos = upper.find("INTO").unwrap_or(0);
    let from_pos = upper.find("FROM").ok_or_else(|| {
        crate::error::OpenSnowError::Internal("COPY INTO requires FROM clause".into())
    })?;

    let table_name = sql[into_pos + 4..from_pos].trim();
    validate_output_table_identifier(table_name, "COPY INTO")?;
    let rest = sql[from_pos + 4..].trim();

    // Extract path (possibly quoted)
    let source_path = if rest.starts_with('\'') || rest.starts_with('"') {
        let quote = rest.chars().next().unwrap();
        let end = rest[1..].find(quote).unwrap_or(rest.len() - 1);
        &rest[1..end + 1]
    } else {
        rest.split_whitespace().next().unwrap_or(rest)
    };

    info!("COPY INTO {} FROM {}", table_name, source_path);

    // Detect format from extension
    let ext = Path::new(source_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("parquet")
        .to_lowercase();

    // Read source data into DataFusion
    let ctx = engine.session_context();
    let temp_name = format!("__copy_source_{}", table_name);

    match ext.as_str() {
        "parquet" | "parq" => {
            ctx.register_parquet(&temp_name, source_path, Default::default())
                .await?;
        }
        "csv" | "tsv" => {
            ctx.register_csv(&temp_name, source_path, Default::default())
                .await?;
        }
        "json" | "ndjson" => {
            ctx.register_json(&temp_name, source_path, Default::default())
                .await?;
        }
        _ => {
            return Err(crate::error::OpenSnowError::Internal(format!(
                "Unsupported file format: .{ext}. Use .parquet, .csv, or .json"
            )));
        }
    }

    // Read all data
    let df = ctx.sql(&format!("SELECT * FROM {temp_name}")).await?;
    let batches = df.collect().await?;

    if batches.is_empty() {
        ctx.deregister_table(&temp_name)?;
        return Ok(vec![]);
    }

    // Write to warehouse as Parquet (local filesystem or object store).
    let schema = batches[0].schema();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    let relative = format!("opensnow/public/{table_name}.parquet");
    let out_path = engine
        .write_warehouse_parquet(&relative, schema, &batches)
        .await
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!("COPY INTO {table_name}: {e:#}"))
        })?;

    // Register the new table
    ctx.deregister_table(&temp_name)?;
    ctx.register_parquet(table_name, &out_path, Default::default())
        .await?;

    info!(
        "COPY INTO {}: loaded {} rows -> {}",
        table_name, total_rows, out_path
    );

    // Return a result message
    let result_schema = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("status", arrow::datatypes::DataType::Utf8, false),
        arrow::datatypes::Field::new("rows_loaded", arrow::datatypes::DataType::Int64, false),
        arrow::datatypes::Field::new("file", arrow::datatypes::DataType::Utf8, false),
    ]));
    let result = RecordBatch::try_new(
        result_schema,
        vec![
            Arc::new(arrow::array::StringArray::from(vec!["LOADED"])),
            Arc::new(arrow::array::Int64Array::from(vec![total_rows as i64])),
            Arc::new(arrow::array::StringArray::from(vec![out_path.as_str()])),
        ],
    )?;

    Ok(vec![result])
}

async fn handle_ctas(engine: &OpenSnowEngine, sql: &str) -> Result<Vec<RecordBatch>> {
    // CREATE TABLE <name> AS <select>
    let upper = sql.to_uppercase();
    let table_pos = keyword_sequence_end(sql, &["CREATE", "TABLE"])
        .unwrap_or_else(|| upper.find("TABLE").unwrap_or(0) + "TABLE".len());
    let (as_pos, query_pos) = find_as_clause(sql, table_pos).ok_or_else(|| {
        crate::error::OpenSnowError::Internal("CREATE TABLE ... AS requires AS clause".into())
    })?;

    let table_name = sql[table_pos..as_pos].trim();
    validate_output_table_identifier(table_name, "CREATE TABLE AS")?;
    let select_sql = validate_materialization_query(&sql[query_pos..], "CREATE TABLE AS")?;

    info!("CREATE TABLE {} AS {}", table_name, select_sql);

    let ctx = engine.session_context();
    let df = ctx.sql(&select_sql).await?;
    let batches = df.collect().await?;

    if batches.is_empty() {
        return Ok(vec![]);
    }

    let schema = batches[0].schema();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    let relative = format!("opensnow/public/{table_name}.parquet");
    let out_path = engine
        .write_warehouse_parquet(&relative, schema, &batches)
        .await
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!("CREATE TABLE {table_name}: {e:#}"))
        })?;

    ctx.register_parquet(table_name, &out_path, Default::default())
        .await?;
    info!(
        "CREATE TABLE {}: {} rows -> {}",
        table_name, total_rows, out_path
    );

    let result_schema = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("status", arrow::datatypes::DataType::Utf8, false),
        arrow::datatypes::Field::new("rows_created", arrow::datatypes::DataType::Int64, false),
    ]));
    let result = RecordBatch::try_new(
        result_schema,
        vec![
            Arc::new(arrow::array::StringArray::from(vec!["CREATED"])),
            Arc::new(arrow::array::Int64Array::from(vec![total_rows as i64])),
        ],
    )?;
    Ok(vec![result])
}

async fn handle_show_tables(engine: &OpenSnowEngine) -> Result<Vec<RecordBatch>> {
    engine.execute_sql_raw(
        "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name"
    ).await
}

async fn handle_show_databases(engine: &OpenSnowEngine) -> Result<Vec<RecordBatch>> {
    engine.execute_sql_raw(
        "SELECT DISTINCT table_catalog AS database_name FROM information_schema.tables ORDER BY 1"
    ).await
}

async fn handle_describe(engine: &OpenSnowEngine, table: &str) -> Result<Vec<RecordBatch>> {
    engine.execute_sql_raw(&format!(
        "SELECT column_name, data_type, is_nullable FROM information_schema.columns WHERE table_name = '{}' ORDER BY ordinal_position",
        table.replace('\'', "")
    )).await
}

// ── Materialized view handlers ──────────────────────────────────────────

/// Execute `select_sql`, write the result as a Parquet file, register it as a
/// table named `name`, and return the row count.
async fn materialize_query_to_parquet(
    engine: &OpenSnowEngine,
    name: &str,
    select_sql: &str,
) -> Result<(usize, String)> {
    let ctx = engine.session_context();
    let df = ctx.sql(select_sql).await?;
    let batches = df.collect().await?;

    let schema = if let Some(b) = batches.first() {
        b.schema()
    } else {
        return Err(crate::error::OpenSnowError::Internal(
            "Materialized view query produced no schema (empty result)".into(),
        ));
    };
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    let relative = format!("opensnow/materialized_views/{name}.parquet");
    let out_path = engine
        .write_warehouse_parquet(&relative, schema, &batches)
        .await
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!("materialized view {name}: {e:#}"))
        })?;

    // Re-register: deregister first to ensure a fresh table reference.
    let _ = ctx.deregister_table(name);
    ctx.register_parquet(name, &out_path, Default::default())
        .await?;

    Ok((total_rows, out_path))
}

async fn handle_create_materialized_view(
    engine: &OpenSnowEngine,
    sql: &str,
) -> Result<Vec<RecordBatch>> {
    // Syntax: CREATE MATERIALIZED VIEW <name> AS <select>
    let header_end =
        keyword_sequence_end(sql, &["CREATE", "MATERIALIZED", "VIEW"]).ok_or_else(|| {
            crate::error::OpenSnowError::Internal("CREATE MATERIALIZED VIEW required".into())
        })?;

    let (as_pos, query_pos) = find_as_clause(sql, header_end).ok_or_else(|| {
        crate::error::OpenSnowError::Internal(
            "CREATE MATERIALIZED VIEW <name> AS <query> requires AS clause".into(),
        )
    })?;

    let name = sql[header_end..as_pos].trim();
    let select_sql = validate_materialization_query(&sql[query_pos..], "CREATE MATERIALIZED VIEW")?;

    if name.is_empty() {
        return Err(crate::error::OpenSnowError::Internal(
            "CREATE MATERIALIZED VIEW requires a name".into(),
        ));
    }
    validate_output_table_identifier(name, "CREATE MATERIALIZED VIEW")?;

    if engine
        .catalog()
        .get_materialized_view(name)
        .map_err(|e| crate::error::OpenSnowError::Internal(format!("catalog error: {e}")))?
        .is_some()
    {
        return Err(crate::error::OpenSnowError::Internal(format!(
            "Materialized view '{name}' already exists"
        )));
    }

    let (total_rows, out_path) = materialize_query_to_parquet(engine, name, &select_sql).await?;

    engine
        .catalog()
        .upsert_materialized_view(name, &select_sql, &out_path)
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!(
                "Failed to record materialized view in catalog: {e}"
            ))
        })?;

    info!(
        "CREATE MATERIALIZED VIEW {}: {} rows -> {}",
        name, total_rows, out_path
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("status", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("rows", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["MATERIALIZED VIEW CREATED"])),
            Arc::new(StringArray::from(vec![name])),
            Arc::new(Int64Array::from(vec![total_rows as i64])),
        ],
    )?;
    Ok(vec![batch])
}

async fn handle_refresh_materialized_view(
    engine: &OpenSnowEngine,
    sql: &str,
) -> Result<Vec<RecordBatch>> {
    // Syntax: REFRESH MATERIALIZED VIEW <name>
    let after_kw = sql["REFRESH MATERIALIZED VIEW".len()..]
        .trim()
        .trim_end_matches(';')
        .trim();
    let name = after_kw.split_whitespace().next().ok_or_else(|| {
        crate::error::OpenSnowError::Internal("REFRESH MATERIALIZED VIEW requires a name".into())
    })?;

    let mv = engine
        .catalog()
        .get_materialized_view(name)
        .map_err(|e| crate::error::OpenSnowError::Internal(format!("catalog error: {e}")))?
        .ok_or_else(|| {
            crate::error::OpenSnowError::Internal(format!("Materialized view '{name}' not found"))
        })?;

    let (total_rows, out_path) = materialize_query_to_parquet(engine, name, &mv.sql).await?;

    engine
        .catalog()
        .upsert_materialized_view(name, &mv.sql, &out_path)
        .map_err(|e| {
            crate::error::OpenSnowError::Internal(format!(
                "Failed to update materialized view in catalog: {e}"
            ))
        })?;

    info!(
        "REFRESH MATERIALIZED VIEW {}: {} rows -> {}",
        name, total_rows, out_path
    );

    let schema = Arc::new(Schema::new(vec![
        Field::new("status", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("rows", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["MATERIALIZED VIEW REFRESHED"])),
            Arc::new(StringArray::from(vec![name])),
            Arc::new(Int64Array::from(vec![total_rows as i64])),
        ],
    )?;
    Ok(vec![batch])
}

async fn handle_drop_materialized_view(
    engine: &OpenSnowEngine,
    sql: &str,
) -> Result<Vec<RecordBatch>> {
    // Syntax: DROP MATERIALIZED VIEW <name>
    let after_kw = sql["DROP MATERIALIZED VIEW".len()..]
        .trim()
        .trim_end_matches(';')
        .trim();
    let name = after_kw.split_whitespace().next().ok_or_else(|| {
        crate::error::OpenSnowError::Internal("DROP MATERIALIZED VIEW requires a name".into())
    })?;

    let mv = engine
        .catalog()
        .get_materialized_view(name)
        .map_err(|e| crate::error::OpenSnowError::Internal(format!("catalog error: {e}")))?;

    let ctx = engine.session_context();
    let _ = ctx.deregister_table(name);

    if let Some(ref mv) = mv {
        let parquet_path = Path::new(&mv.parquet_path);
        let remove_result = parquet_path
            .exists()
            .then(|| std::fs::remove_file(parquet_path));
        if let Some(Err(e)) = remove_result {
            tracing::warn!(
                "Failed to delete materialized view parquet '{}': {}",
                mv.parquet_path,
                e
            );
        }
    }

    let removed = engine
        .catalog()
        .delete_materialized_view(name)
        .map_err(|e| crate::error::OpenSnowError::Internal(format!("catalog error: {e}")))?;

    if !removed && mv.is_none() {
        return Err(crate::error::OpenSnowError::Internal(format!(
            "Materialized view '{name}' not found"
        )));
    }

    info!("DROP MATERIALIZED VIEW {}", name);

    let schema = Arc::new(Schema::new(vec![
        Field::new("status", DataType::Utf8, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec!["MATERIALIZED VIEW DROPPED"])),
            Arc::new(StringArray::from(vec![name])),
        ],
    )?;
    Ok(vec![batch])
}

async fn handle_drop_table(engine: &OpenSnowEngine, table: &str) -> Result<Vec<RecordBatch>> {
    let clean = table.replace(['\'', '"'], "");
    let ctx = engine.session_context();

    // Deregister from DataFusion
    ctx.deregister_table(&clean)?;

    // Remove parquet file
    let warehouse = engine.warehouse_path();
    let file_path = format!("{warehouse}/opensnow/public/{clean}.parquet");
    if Path::new(&file_path).exists() {
        std::fs::remove_file(&file_path)?;
    }

    info!("Dropped table: {}", clean);

    let schema = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("status", arrow::datatypes::DataType::Utf8, false),
    ]));
    let result = RecordBatch::try_new(
        schema,
        vec![Arc::new(arrow::array::StringArray::from(vec!["DROPPED"]))],
    )?;
    Ok(vec![result])
}

#[cfg(test)]
mod tests {
    use crate::engine::{EngineConfig, OpenSnowEngine};

    /// Build an isolated engine using a tempdir for both warehouse + catalog.
    fn isolated_engine() -> (OpenSnowEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let warehouse = dir.path().join("warehouse");
        std::fs::create_dir_all(&warehouse).unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let config = EngineConfig {
            warehouse_path: warehouse.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let engine =
            OpenSnowEngine::from_config_and_catalog(config, catalog_path.to_str().unwrap());
        (engine, dir)
    }

    #[tokio::test]
    async fn reject_invalid_warehouse_names_before_catalog_mutation() {
        let (engine, _dir) = isolated_engine();

        for sql in [
            "CREATE WAREHOUSE ../escape WITH SIZE = 'small'",
            "CREATE WAREHOUSE public.bad WITH SIZE = 'small'",
            "CREATE WAREHOUSE 9bad WITH SIZE = 'small'",
            "CREATE WAREHOUSE \"quoted\" WITH SIZE = 'small'",
        ] {
            let err = engine.execute_sql(sql).await.unwrap_err().to_string();
            assert!(
                err.contains("Invalid CREATE WAREHOUSE target identifier"),
                "expected warehouse identifier validation error for {sql}, got: {err}"
            );
        }

        let warehouses = engine.catalog().list_warehouses().unwrap();
        assert!(
            warehouses
                .iter()
                .all(|warehouse| warehouse.name == "default"),
            "invalid warehouse names must be rejected before catalog mutation: {warehouses:?}"
        );
    }

    #[tokio::test]
    async fn reject_invalid_warehouse_resource_options_before_catalog_mutation() {
        let (engine, _dir) = isolated_engine();

        for sql in [
            "CREATE WAREHOUSE bad_size WITH SIZE = 'planet'",
            "CREATE WAREHOUSE bad_min WITH MIN_NODES = -1",
            "CREATE WAREHOUSE bad_max WITH MAX_NODES = -1",
            "CREATE WAREHOUSE bad_order WITH MIN_NODES = 3 MAX_NODES = 2",
            "CREATE WAREHOUSE bad_suspend WITH AUTO_SUSPEND = -10",
            "CREATE WAREHOUSE bad_parse WITH MIN_NODES = nope",
        ] {
            let err = engine.execute_sql(sql).await.unwrap_err().to_string();
            assert!(
                err.contains("Invalid CREATE WAREHOUSE"),
                "expected warehouse option validation error for {sql}, got: {err}"
            );
        }

        let names: Vec<_> = engine
            .catalog()
            .list_warehouses()
            .unwrap()
            .into_iter()
            .map(|warehouse| warehouse.name)
            .collect();
        assert_eq!(names, vec!["default".to_string()]);
    }

    #[tokio::test]
    async fn reject_path_traversal_output_identifiers_before_file_io() {
        let (engine, _dir) = isolated_engine();

        for sql in [
            "CREATE TABLE ../escape AS SELECT 1 AS x",
            "CREATE TABLE public.bad AS SELECT 1 AS x",
            "COPY INTO ../escape FROM '/tmp/missing.parquet'",
            "COPY INTO public.bad FROM '/tmp/missing.parquet'",
            "CREATE MATERIALIZED VIEW ../escape AS SELECT 1 AS x",
            "CREATE MATERIALIZED VIEW public.bad AS SELECT 1 AS x",
        ] {
            let err = engine.execute_sql(sql).await.unwrap_err().to_string();
            assert!(
                err.contains("Invalid") && err.contains("target identifier"),
                "expected identifier validation error for {sql}, got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn reject_destructive_queries_wrapped_by_materializing_commands_before_side_effects() {
        let (engine, _dir) = isolated_engine();

        for sql in [
            "CREATE TABLE safe AS DROP TABLE victim",
            "CREATE TABLE safe_tab AS\tDROP TABLE victim",
            "CREATE TABLE safe_newline AS\nDROP TABLE victim",
            "CREATE TABLE safe AS DELETE FROM victim",
            "CREATE TABLE safe_tab AS\tDELETE FROM victim",
            "CREATE TABLE safe_newline AS\nDELETE FROM victim",
            "CREATE MATERIALIZED VIEW safe_mv AS DROP TABLE victim",
            "CREATE MATERIALIZED VIEW safe_mv_tab AS\tDROP TABLE victim",
            "CREATE MATERIALIZED VIEW safe_mv_newline AS\nDROP TABLE victim",
            "CREATE MATERIALIZED VIEW safe_mv AS DELETE FROM victim",
            "CREATE MATERIALIZED VIEW safe_mv_tab AS\tDELETE FROM victim",
            "CREATE MATERIALIZED VIEW safe_mv_newline AS\nDELETE FROM victim",
        ] {
            engine
                .execute_sql("CREATE TABLE victim AS SELECT 1 AS x")
                .await
                .unwrap();

            let err = engine.execute_sql(sql).await.unwrap_err().to_string();
            assert!(
                err.contains("materialization query") && err.contains("SELECT or WITH"),
                "expected materialization query validation error for {sql}, got: {err}"
            );

            let victim = engine.execute_sql("SELECT x FROM victim").await.unwrap();
            assert_eq!(
                victim[0].num_rows(),
                1,
                "victim table must remain after {sql}"
            );

            engine.execute_sql("DROP TABLE victim").await.unwrap();
        }
    }

    #[tokio::test]
    async fn accept_materializing_commands_with_whitespace_after_as_keyword() {
        let (engine, _dir) = isolated_engine();

        for (sql, table_name) in [
            ("CREATE TABLE ctas_tab AS\tSELECT 1 AS x", "ctas_tab"),
            (
                "CREATE TABLE ctas_newline AS\nSELECT 1 AS x",
                "ctas_newline",
            ),
            (
                "CREATE MATERIALIZED VIEW mv_tab AS\tSELECT 1 AS x",
                "mv_tab",
            ),
            (
                "CREATE MATERIALIZED VIEW mv_newline AS\nSELECT 1 AS x",
                "mv_newline",
            ),
        ] {
            engine.execute_sql(sql).await.unwrap();
            let rows = engine
                .execute_sql(&format!("SELECT x FROM {table_name}"))
                .await
                .unwrap();
            assert_eq!(rows[0].num_rows(), 1, "expected queryable output for {sql}");
        }
    }

    #[tokio::test]
    async fn test_create_materialized_view() {
        let (engine, _dir) = isolated_engine();

        let result = engine
            .execute_sql("CREATE MATERIALIZED VIEW mv_demo AS SELECT 42 AS answer")
            .await
            .unwrap();
        assert_eq!(result[0].num_rows(), 1);

        // Catalog row exists
        let mv = engine.catalog().get_materialized_view("mv_demo").unwrap();
        assert!(mv.is_some());
        let mv = mv.unwrap();
        assert!(std::path::Path::new(&mv.parquet_path).exists());
        assert_eq!(mv.sql.trim(), "SELECT 42 AS answer");

        // The MV is queryable as a regular table
        let q = engine
            .execute_sql("SELECT answer FROM mv_demo")
            .await
            .unwrap();
        assert_eq!(q[0].num_rows(), 1);

        // Re-creating with the same name should fail
        let err = engine
            .execute_sql("CREATE MATERIALIZED VIEW mv_demo AS SELECT 1")
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_refresh_materialized_view() {
        let (engine, _dir) = isolated_engine();

        engine
            .execute_sql(
                "CREATE MATERIALIZED VIEW mv_count AS SELECT COUNT(*) AS cnt FROM (VALUES (1), (2)) AS t(x)",
            )
            .await
            .unwrap();

        let q1 = engine
            .execute_sql("SELECT cnt FROM mv_count")
            .await
            .unwrap();
        assert_eq!(q1[0].num_rows(), 1);

        let before = engine
            .catalog()
            .get_materialized_view("mv_count")
            .unwrap()
            .unwrap()
            .last_refreshed;

        // Wait long enough for the rfc3339 timestamp (millisecond precision) to differ.
        std::thread::sleep(std::time::Duration::from_millis(5));

        engine
            .execute_sql("REFRESH MATERIALIZED VIEW mv_count")
            .await
            .unwrap();

        let after = engine
            .catalog()
            .get_materialized_view("mv_count")
            .unwrap()
            .unwrap()
            .last_refreshed;
        assert_ne!(before, after);

        // Still queryable
        let q2 = engine
            .execute_sql("SELECT cnt FROM mv_count")
            .await
            .unwrap();
        assert_eq!(q2[0].num_rows(), 1);

        // Refreshing a nonexistent MV is an error
        let err = engine.execute_sql("REFRESH MATERIALIZED VIEW nope").await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_drop_materialized_view() {
        let (engine, _dir) = isolated_engine();

        engine
            .execute_sql("CREATE MATERIALIZED VIEW mv_drop AS SELECT 1 AS x")
            .await
            .unwrap();

        let parquet_path = engine
            .catalog()
            .get_materialized_view("mv_drop")
            .unwrap()
            .unwrap()
            .parquet_path;
        assert!(std::path::Path::new(&parquet_path).exists());

        engine
            .execute_sql("DROP MATERIALIZED VIEW mv_drop")
            .await
            .unwrap();

        // Catalog row is gone
        assert!(
            engine
                .catalog()
                .get_materialized_view("mv_drop")
                .unwrap()
                .is_none()
        );
        // Parquet file is removed
        assert!(!std::path::Path::new(&parquet_path).exists());
        // Querying the table now fails
        let err = engine.execute_sql("SELECT * FROM mv_drop").await;
        assert!(err.is_err());

        // Dropping a non-existent MV is an error
        let err = engine.execute_sql("DROP MATERIALIZED VIEW nope").await;
        assert!(err.is_err());
    }
}
