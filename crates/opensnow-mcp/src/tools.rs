/// Richer MCP tool endpoints:
///   POST /tools/propose_schema
///   POST /tools/propose_migration
///   POST /tools/safe_run_sql
use axum::{
    Json,
    extract::{Extension, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::AppState;
use crate::auth::AuthConfig;
use opensnow_auth::Privilege;

// ── Auth helper ───────────────────────────────────────────────────────────────

fn token_from_headers(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

// ── Request/Response types ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ProposeSchemaRequest {
    pub table_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProposeSchemaResponse {
    pub tables: Vec<TableProposal>,
}

#[derive(Debug, Serialize)]
pub struct TableProposal {
    pub table: String,
    pub proposed_columns: Vec<String>,
    pub notes: String,
}

#[derive(Debug, Deserialize)]
pub struct ProposeMigrationRequest {
    pub target_table: String,
}

#[derive(Debug, Serialize)]
pub struct ProposeMigrationResponse {
    pub target_table: String,
    pub ctas_sql: String,
    pub estimated_rows: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct SafeRunSqlRequest {
    pub sql: String,
}

#[derive(Debug, Serialize)]
pub struct SafeRunSqlResponse {
    pub status: String,
    pub rows: usize,
    pub data: String,
    pub error: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// POST /tools/propose_schema
pub async fn propose_schema(
    State(handle): State<AppState>,
    Extension(auth_config): Extension<AuthConfig>,
    headers: HeaderMap,
    Json(req): Json<ProposeSchemaRequest>,
) -> Result<Json<ProposeSchemaResponse>, (StatusCode, String)> {
    if auth_config.jwt_mode_enabled() {
        crate::auth::authorize_headers_with_config(
            &headers,
            &auth_config,
            &[],
            &["table.select", "policy.admin"],
        )
        .map_err(|status| (status, "insufficient scope".to_string()))?;
        if let Some(name) = req.table_name.as_deref() {
            if !crate::is_safe_table_identifier(name) {
                return Err((StatusCode::BAD_REQUEST, "invalid table name".to_string()));
            }
            if !crate::authorize_mcp_table(&headers, &auth_config, name, Privilege::Select)
                .map_err(|status| (status, "object policy denied".to_string()))?
            {
                return Err((StatusCode::FORBIDDEN, "object policy denied".to_string()));
            }
        }
    } else if !auth_config.can_write_token(&token_from_headers(&headers)) {
        return Err((StatusCode::FORBIDDEN, "insufficient role".to_string()));
    }

    let sql = match &req.table_name {
        Some(name) => format!(
            "SELECT table_name, column_name, data_type, is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = '{}' \
             ORDER BY ordinal_position",
            name.replace('\'', "")
        ),
        None => "SELECT table_name, column_name, data_type, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' \
                 ORDER BY table_name, ordinal_position"
            .to_string(),
    };

    let batches = handle.execute_sql(&sql).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("schema query failed: {e}"),
        )
    })?;

    let mut map: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut nullable_map: std::collections::BTreeMap<String, Vec<bool>> = Default::default();

    for batch in &batches {
        for i in 0..batch.num_rows() {
            let tbl =
                arrow::util::display::array_value_to_string(batch.column(0), i).unwrap_or_default();
            let col =
                arrow::util::display::array_value_to_string(batch.column(1), i).unwrap_or_default();
            let dtype =
                arrow::util::display::array_value_to_string(batch.column(2), i).unwrap_or_default();
            let nullable = arrow::util::display::array_value_to_string(batch.column(3), i)
                .unwrap_or_default()
                == "YES";
            map.entry(tbl.clone())
                .or_default()
                .push(format!("{col} {dtype}"));
            nullable_map.entry(tbl).or_default().push(nullable);
        }
    }

    let mut tables = Vec::new();
    for (table, cols) in map {
        if auth_config.jwt_mode_enabled()
            && !crate::authorize_mcp_table(&headers, &auth_config, &table, Privilege::Select)
                .map_err(|status| (status, "object policy denied".to_string()))?
        {
            continue;
        }
        let nc = nullable_map
            .get(&table)
            .map(|v| v.iter().filter(|&&n| n).count())
            .unwrap_or(0);
        tables.push(TableProposal {
            table,
            proposed_columns: cols,
            notes: if nc > 0 {
                format!("{nc} nullable column(s) — consider NOT NULL constraints")
            } else {
                "all columns are NOT NULL".to_string()
            },
        });
    }

    Ok(Json(ProposeSchemaResponse { tables }))
}

/// POST /tools/propose_migration — generates CTAS SQL, does not execute.
pub async fn propose_migration(
    State(_handle): State<AppState>,
    Extension(auth_config): Extension<AuthConfig>,
    headers: HeaderMap,
    Json(req): Json<ProposeMigrationRequest>,
) -> Result<Json<ProposeMigrationResponse>, (StatusCode, String)> {
    let safe_name: String = req
        .target_table
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_')
        .collect();

    if safe_name.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "invalid table name".to_string()));
    }
    let target_table = format!("{safe_name}_mart");

    if auth_config.jwt_mode_enabled() {
        crate::auth::authorize_headers_with_config(
            &headers,
            &auth_config,
            &[],
            &["table.create", "policy.admin"],
        )
        .map_err(|status| (status, "insufficient scope".to_string()))?;
        let source_allowed =
            crate::authorize_mcp_table(&headers, &auth_config, &safe_name, Privilege::Select)
                .map_err(|status| (status, "object policy denied".to_string()))?;
        let target_allowed =
            crate::authorize_mcp_table(&headers, &auth_config, &target_table, Privilege::Create)
                .map_err(|status| (status, "object policy denied".to_string()))?;
        if !source_allowed || !target_allowed {
            return Err((StatusCode::FORBIDDEN, "object policy denied".to_string()));
        }
    } else if !auth_config.can_write_token(&token_from_headers(&headers)) {
        return Err((StatusCode::FORBIDDEN, "insufficient role".to_string()));
    }

    let ctas_sql = format!("CREATE TABLE {target_table} AS\n  SELECT * FROM {safe_name};");
    info!("propose_migration: {ctas_sql}");

    Ok(Json(ProposeMigrationResponse {
        target_table: safe_name,
        ctas_sql,
        estimated_rows: None,
    }))
}

/// POST /tools/safe_run_sql — SELECT/WITH only.
pub async fn safe_run_sql(
    State(handle): State<AppState>,
    Extension(auth_config): Extension<AuthConfig>,
    headers: HeaderMap,
    Json(req): Json<SafeRunSqlRequest>,
) -> Response {
    if auth_config.jwt_mode_enabled() {
        if let Err(status) = crate::auth::authorize_headers_with_config(
            &headers,
            &auth_config,
            &["sql.query", "table.select"],
            &[],
        ) {
            return (
                status,
                Json(SafeRunSqlResponse {
                    status: "forbidden".to_string(),
                    rows: 0,
                    data: String::new(),
                    error: Some("insufficient scope".to_string()),
                }),
            )
                .into_response();
        }
    } else if !auth_config.can_write_token(&token_from_headers(&headers)) {
        return (
            StatusCode::FORBIDDEN,
            Json(SafeRunSqlResponse {
                status: "forbidden".to_string(),
                rows: 0,
                data: String::new(),
                error: Some("insufficient role".to_string()),
            }),
        )
            .into_response();
    }

    let trimmed = req.sql.trim().to_lowercase();
    if !trimmed.starts_with("select") && !trimmed.starts_with("with") {
        return (
            StatusCode::BAD_REQUEST,
            Json(SafeRunSqlResponse {
                status: "rejected".to_string(),
                rows: 0,
                data: String::new(),
                error: Some("only SELECT/WITH statements are permitted".to_string()),
            }),
        )
            .into_response();
    }

    if auth_config.jwt_mode_enabled() {
        if let Err(status) = crate::authorize_mcp_sql(&headers, &auth_config, &req.sql) {
            return (
                status,
                Json(SafeRunSqlResponse {
                    status: "forbidden".to_string(),
                    rows: 0,
                    data: String::new(),
                    error: Some("object policy denied".to_string()),
                }),
            )
                .into_response();
        }
    }

    match handle.execute_sql(&req.sql).await {
        Ok(batches) => {
            let rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            let data = batches
                .first()
                .map(|batch| {
                    let buf = Vec::new();
                    let mut w = arrow::json::LineDelimitedWriter::new(buf);
                    w.write(batch).ok();
                    w.finish().ok();
                    String::from_utf8(w.into_inner()).unwrap_or_default()
                })
                .unwrap_or_default();
            (
                StatusCode::OK,
                Json(SafeRunSqlResponse {
                    status: "ok".to_string(),
                    rows,
                    data,
                    error: None,
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(SafeRunSqlResponse {
                status: "error".to_string(),
                rows: 0,
                data: String::new(),
                error: Some(e.to_string()),
            }),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn safe_name_sanitisation() {
        let dirty = "fact_orders; DROP TABLE users;";
        let clean: String = dirty
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        assert!(!clean.contains(';'));
        assert!(!clean.contains(' '));
    }

    #[test]
    fn reject_non_select_sql() {
        let sql = "DROP TABLE users";
        let t = sql.trim().to_lowercase();
        assert!(!t.starts_with("select") && !t.starts_with("with"));
    }
}
