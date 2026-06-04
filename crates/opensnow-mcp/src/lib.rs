#![allow(
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::too_many_arguments,
    clippy::question_mark,
    clippy::items_after_test_module,
    clippy::bool_assert_comparison,
    clippy::await_holding_lock,
    clippy::field_reassign_with_default
)]
use std::net::SocketAddr;

use anyhow::Result;
use arrow::array::RecordBatch;
use axum::extract::{Extension, Path, State};
use axum::middleware;
use axum::routing::{get, post};
use axum::{Json, Router};
use opensnow_auth::{ObjectType, Privilege};
use opensnow_core::{EngineConfig, OpenSnowEngine};
use serde::{Deserialize, Serialize};
use tracing::info;

pub mod auth;
pub mod engine_handle;
pub mod tools;

use engine_handle::EngineHandle;

/// Shared application state — Send + Sync via EngineHandle.
pub type AppState = EngineHandle;

#[derive(Debug, Serialize)]
pub struct TableInfo {
    pub database: String,
    pub schema: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct TablesResponse {
    pub tables: Vec<TableInfo>,
}

#[derive(Debug, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub data_type: String,
}

#[derive(Debug, Serialize)]
pub struct TableDetailResponse {
    pub table: String,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Debug, Serialize)]
pub struct QueryResponse {
    pub status: String,
    pub rows: usize,
    pub data: String,
}

/// Build the MCP HTTP router.
pub fn router(handle: EngineHandle) -> Router {
    router_with_auth(handle, auth::AuthConfig::from_env())
}

/// Build the MCP HTTP router with a fixed auth snapshot.
pub fn router_with_auth(handle: EngineHandle, auth_config: auth::AuthConfig) -> Router {
    Router::new()
        .route("/schema/tables", get(list_tables))
        .route("/schema/tables/{table}", get(table_detail))
        .route("/query", post(run_query))
        .route("/tools/propose_schema", post(tools::propose_schema))
        .route("/tools/propose_migration", post(tools::propose_migration))
        .route("/tools/safe_run_sql", post(tools::safe_run_sql))
        .route("/agent/tasks/{task_name}", post(run_agent_task))
        .nest(
            "/agent/v1",
            opensnow_agent::metadata_api::create_agent_router(),
        )
        .layer(middleware::from_fn(auth::require_auth))
        .layer(Extension(auth_config))
        .with_state(handle)
}

async fn list_tables(
    State(handle): State<AppState>,
    Extension(auth_config): Extension<auth::AuthConfig>,
    headers: axum::http::HeaderMap,
) -> Result<Json<TablesResponse>, axum::http::StatusCode> {
    let mut tables_out = Vec::new();
    match handle.list_tables("opensnow", "public").await {
        Ok(tables) => {
            for (name, _location) in tables {
                if !authorize_mcp_table(&headers, &auth_config, &name, Privilege::Select)? {
                    continue;
                }
                tables_out.push(TableInfo {
                    database: "opensnow".to_string(),
                    schema: "public".to_string(),
                    name,
                });
            }
        }
        Err(e) => {
            tracing::warn!("failed to list tables: {}", e);
        }
    }
    Ok(Json(TablesResponse { tables: tables_out }))
}

async fn table_detail(
    State(handle): State<AppState>,
    Path(table): Path<String>,
    Extension(auth_config): Extension<auth::AuthConfig>,
    headers: axum::http::HeaderMap,
) -> Result<Json<TableDetailResponse>, axum::http::StatusCode> {
    if !is_safe_table_identifier(&table) {
        return Err(axum::http::StatusCode::BAD_REQUEST);
    }
    if !authorize_mcp_table(&headers, &auth_config, &table, Privilege::Select)? {
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    let sql = format!(
        "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = '{}'",
        table
    );
    let mut columns = Vec::new();
    match handle.execute_sql(&sql).await {
        Ok(batches) => {
            for batch in &batches {
                columns.extend(extract_columns_from_batch(batch));
            }
        }
        Err(e) => {
            tracing::warn!("failed to fetch column metadata for {}: {}", table, e);
        }
    }
    Ok(Json(TableDetailResponse { table, columns }))
}

pub(crate) fn authorize_mcp_sql(
    headers: &axum::http::HeaderMap,
    config: &auth::AuthConfig,
    sql: &str,
) -> Result<(), axum::http::StatusCode> {
    let Some(claims) = auth::claims_from_headers_with_config(headers, config)? else {
        return Ok(());
    };
    if claims.role == "ACCOUNTADMIN"
        || claims.role == "SYSADMIN"
        || claims
            .scopes
            .iter()
            .any(|s| s == "*" || s == "policy.admin")
    {
        return Ok(());
    }
    let requirements = analyze_mcp_sql(sql).map_err(|_| axum::http::StatusCode::FORBIDDEN)?;
    if requirements.is_empty() {
        return if is_safe_objectless_query(sql) {
            Ok(())
        } else {
            Err(axum::http::StatusCode::FORBIDDEN)
        };
    }
    let Some(policy) = config.object_policy() else {
        return Err(axum::http::StatusCode::FORBIDDEN);
    };
    for (privilege, object_type, object_name) in requirements {
        if !policy
            .check_privilege(&claims.role, privilege, object_type, &object_name)
            .map_err(|_| axum::http::StatusCode::FORBIDDEN)?
        {
            return Err(axum::http::StatusCode::FORBIDDEN);
        }
    }
    Ok(())
}

pub(crate) fn authorize_mcp_table(
    headers: &axum::http::HeaderMap,
    config: &auth::AuthConfig,
    table: &str,
    privilege: Privilege,
) -> Result<bool, axum::http::StatusCode> {
    let Some(claims) = auth::claims_from_headers_with_config(headers, config)? else {
        return Ok(true);
    };
    if claims.role == "ACCOUNTADMIN"
        || claims.role == "SYSADMIN"
        || claims
            .scopes
            .iter()
            .any(|s| s == "*" || s == "policy.admin")
    {
        return Ok(true);
    }
    let Some(policy) = config.object_policy() else {
        return Ok(false);
    };
    if !is_safe_table_identifier(table) {
        return Ok(false);
    }
    policy
        .check_privilege(&claims.role, privilege, ObjectType::Table, table)
        .map_err(|_| axum::http::StatusCode::FORBIDDEN)
}

fn analyze_mcp_sql(sql: &str) -> Result<Vec<(Privilege, ObjectType, String)>, ()> {
    let tokens = tokenize_sql(sql);
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let mut reqs = Vec::new();
    match tokens[0].to_ascii_uppercase().as_str() {
        "SELECT" | "WITH" => collect_select_reqs(&tokens, &mut reqs)?,
        "INSERT" => {
            if let Some(idx) = tokens.iter().position(|t| t.eq_ignore_ascii_case("INTO")) {
                let name = next_sql_object(&tokens, idx + 1)?.ok_or(())?;
                push_table_req(&mut reqs, Privilege::Insert, name)?;
            }
            collect_select_reqs(&tokens, &mut reqs)?;
        }
        "CREATE" | "DROP" | "ALTER" => {
            let privilege = match tokens[0].to_ascii_uppercase().as_str() {
                "CREATE" => Privilege::Create,
                "DROP" => Privilege::Drop,
                _ => Privilege::Alter,
            };
            if tokens
                .get(1)
                .is_some_and(|kind| kind.eq_ignore_ascii_case("TABLE"))
            {
                let name = next_sql_object(&tokens, 2)?.ok_or(())?;
                push_table_req(&mut reqs, privilege, name)?;
            }
            collect_select_reqs(&tokens, &mut reqs)?;
        }
        _ => {}
    }
    Ok(reqs)
}

fn is_safe_objectless_query(sql: &str) -> bool {
    tokenize_sql(sql)
        .first()
        .is_some_and(|t| matches!(t.to_ascii_uppercase().as_str(), "SELECT" | "WITH"))
}

fn collect_select_reqs(
    tokens: &[String],
    reqs: &mut Vec<(Privilege, ObjectType, String)>,
) -> Result<(), ()> {
    for (idx, token) in tokens.iter().enumerate() {
        if matches!(token.to_ascii_uppercase().as_str(), "FROM" | "JOIN") {
            let name = next_sql_object(tokens, idx + 1)?.ok_or(())?;
            push_table_req(reqs, Privilege::Select, name)?;
        }
    }
    Ok(())
}

fn push_table_req(
    reqs: &mut Vec<(Privilege, ObjectType, String)>,
    privilege: Privilege,
    raw: &str,
) -> Result<(), ()> {
    let name = raw
        .trim_matches('"')
        .split('.')
        .next_back()
        .unwrap_or(raw)
        .trim_matches('"')
        .to_string();
    if !is_safe_table_identifier(&name) {
        return Err(());
    }
    if !reqs.iter().any(|r| r.0 == privilege && r.2 == name) {
        reqs.push((privilege, ObjectType::Table, name));
    }
    Ok(())
}

fn next_sql_object(tokens: &[String], mut idx: usize) -> Result<Option<&str>, ()> {
    while let Some(token) = tokens.get(idx) {
        if matches!(
            token.to_ascii_uppercase().as_str(),
            "IF" | "NOT" | "EXISTS" | "ONLY"
        ) {
            idx += 1;
            continue;
        }
        if token
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '"')
        {
            return Ok(Some(token));
        }
        return Err(());
    }
    Ok(None)
}

fn tokenize_sql(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '-' && chars.peek() == Some(&'-') {
            chars.next();
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            for comment_ch in chars.by_ref() {
                if comment_ch == '\n' {
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            let mut previous = '\0';
            for comment_ch in chars.by_ref() {
                if previous == '*' && comment_ch == '/' {
                    break;
                }
                previous = comment_ch;
            }
            continue;
        }
        if ch.is_whitespace() || matches!(ch, ',' | ';' | '(' | ')') {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

pub(crate) fn is_safe_table_identifier(table: &str) -> bool {
    !table.is_empty()
        && table
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && table
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}

fn extract_columns_from_batch(batch: &RecordBatch) -> Vec<ColumnInfo> {
    if batch.num_columns() < 2 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let name =
            arrow::util::display::array_value_to_string(batch.column(0), row).unwrap_or_default();
        let data_type =
            arrow::util::display::array_value_to_string(batch.column(1), row).unwrap_or_default();
        if !name.is_empty() {
            out.push(ColumnInfo { name, data_type });
        }
    }
    out
}

async fn run_query(
    State(handle): State<AppState>,
    Extension(auth_config): Extension<auth::AuthConfig>,
    headers: axum::http::HeaderMap,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, axum::http::StatusCode> {
    auth::authorize_headers_with_config(
        &headers,
        &auth_config,
        &["sql.query", "table.select"],
        &[],
    )?;
    authorize_mcp_sql(&headers, &auth_config, &req.sql)?;
    info!("MCP query: {}", req.sql);
    match handle.execute_sql(&req.sql).await {
        Ok(batches) => {
            let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            let data = if let Some(batch) = batches.first() {
                let buf = Vec::new();
                let mut writer = arrow::json::LineDelimitedWriter::new(buf);
                writer.write(batch).ok();
                writer.finish().ok();
                String::from_utf8(writer.into_inner()).unwrap_or_default()
            } else {
                String::new()
            };
            Ok(Json(QueryResponse {
                status: "ok".to_string(),
                rows: total_rows,
                data,
            }))
        }
        Err(e) => Ok(Json(QueryResponse {
            status: format!("error: {}", e),
            rows: 0,
            data: String::new(),
        })),
    }
}

#[derive(Deserialize)]
struct AgentTaskRequest {
    #[serde(default)]
    params: serde_json::Value,
}

async fn run_agent_task(
    State(_handle): State<AppState>,
    Path(task_name): Path<String>,
    Json(req): Json<AgentTaskRequest>,
) -> Json<serde_json::Value> {
    // OpenSnowEngine is !Send, so we run the agent task on a dedicated thread
    // with its own single-threaded Tokio runtime (same pattern as EngineHandle).
    let result = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build agent task runtime");
        rt.block_on(async {
            let engine = OpenSnowEngine::with_config(EngineConfig::default());
            opensnow_agent::run_task(&task_name, engine, req.params).await
        })
    })
    .await;

    match result {
        Ok(Ok(report)) => Json(serde_json::json!({"status": "ok", "report": report})),
        Ok(Err(e)) => Json(serde_json::json!({"status": "error", "message": e.to_string()})),
        Err(e) => {
            Json(serde_json::json!({"status": "error", "message": format!("task panicked: {e}")}))
        }
    }
}

/// Start the MCP HTTP server.
pub async fn serve(engine: OpenSnowEngine, addr: SocketAddr) -> Result<()> {
    let handle = EngineHandle::spawn(engine);
    let app = router(handle);
    info!("starting MCP HTTP server on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http::{Request, Response, StatusCode};
    use http_body_util::BodyExt;
    use opensnow_core::{EngineConfig, OpenSnowEngine};
    use std::sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    };
    use tower::ServiceExt;

    // Serialize tests that mutate env vars so parallel runs don't interfere.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static TEST_ENGINE_ID: AtomicU64 = AtomicU64::new(0);
    const TEST_ADMIN_TOKEN: &str = "mcp_test_admin_token";

    fn test_app_with_auth(auth_config: auth::AuthConfig) -> axum::Router {
        let id = TEST_ENGINE_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("opensnow-mcp-test-{}-{id}", std::process::id()));
        let mut config = EngineConfig::default();
        config.warehouse_path = root.join("warehouse").to_string_lossy().into_owned();
        let catalog_path = root.join("catalog.db").to_string_lossy().into_owned();
        let engine = OpenSnowEngine::from_config_and_catalog(config, &catalog_path);
        let handle = EngineHandle::spawn(engine);
        router_with_auth(handle, auth_config)
    }

    fn test_app() -> axum::Router {
        test_app_with_auth(auth::AuthConfig::disabled())
    }

    fn test_app_as_admin() -> axum::Router {
        test_app_with_auth(auth::AuthConfig::disabled().with_role_token(TEST_ADMIN_TOKEN, "admin"))
    }

    fn test_app_with_jwt(secret: &str) -> axum::Router {
        test_app_with_auth(auth::AuthConfig::jwt(secret))
    }

    fn test_app_with_jwt_and_policy(
        secret: &str,
        policy: opensnow_auth::PrivilegeStore,
    ) -> axum::Router {
        test_app_with_auth(auth::AuthConfig::jwt(secret).with_object_policy(policy))
    }

    fn test_policy() -> opensnow_auth::PrivilegeStore {
        opensnow_auth::PrivilegeStore::new(std::sync::Arc::new(std::sync::Mutex::new(
            rusqlite::Connection::open_in_memory().unwrap(),
        )))
        .unwrap()
    }

    async fn body_json(resp: Response<Body>) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn do_get(path: &str) -> Response<Body> {
        let app = test_app();
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    async fn do_post(path: &str, payload: &'static str) -> Response<Body> {
        let app = test_app();
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    async fn do_post_as_admin(path: &str, payload: &'static str) -> Response<Body> {
        let app = test_app_as_admin();
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(Body::from(payload))
            .unwrap();
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    async fn do_post_with_bearer(path: &str, payload: &'static str, token: &str) -> Response<Body> {
        let app = test_app_with_jwt("mcp-jwt-secret");
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(payload))
            .unwrap();
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    async fn do_post_with_bearer_and_policy(
        path: &str,
        payload: &'static str,
        token: &str,
        policy: opensnow_auth::PrivilegeStore,
    ) -> Response<Body> {
        let app = test_app_with_jwt_and_policy("mcp-jwt-secret", policy);
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::from(payload))
            .unwrap();
        ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn get_schema_tables_returns_200() {
        let resp = do_get("/schema/tables").await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(json["tables"].is_array());
    }

    #[tokio::test]
    async fn post_query_returns_ok_status() {
        let resp = do_post("/query", r#"{"sql":"SELECT 1 AS n"}"#).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"].as_str(), Some("ok"));
    }

    #[tokio::test]
    async fn router_auth_mode_is_snapshotted_at_construction() {
        let _lock = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("MCP_JWT_SECRET") };
        unsafe { std::env::remove_var("MCP_AUTH_TOKEN") };
        let app = test_app();
        unsafe { std::env::set_var("MCP_JWT_SECRET", "mcp-jwt-secret") };

        let req = Request::builder()
            .method("POST")
            .uri("/query")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"sql":"SELECT 1 AS n"}"#))
            .unwrap();
        let resp = ServiceExt::<Request<Body>>::oneshot(app, req)
            .await
            .unwrap();
        unsafe { std::env::remove_var("MCP_JWT_SECRET") };
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn post_query_with_jwt_requires_sql_and_table_scopes() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let weak = jwt
            .generate_token_with_scopes(
                7,
                "analyst",
                "ANALYST",
                "default",
                vec!["profile.read".to_string()],
                1,
            )
            .unwrap();
        let scoped = jwt
            .generate_token_with_scopes(
                8,
                "reader",
                "ANALYST",
                "default",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();

        let denied = do_post_with_bearer("/query", r#"{"sql":"SELECT 1 AS n"}"#, &weak).await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let allowed = do_post_with_bearer("/query", r#"{"sql":"SELECT 1 AS n"}"#, &scoped).await;
        assert_eq!(allowed.status(), StatusCode::OK);
        let json = body_json(allowed).await;
        assert_eq!(json["status"].as_str(), Some("ok"));
    }

    #[tokio::test]
    async fn tools_without_auth_are_forbidden() {
        // No MCP_TOKEN_ADMIN set — any token (or none) gets 403.
        let resp = do_post("/tools/safe_run_sql", r#"{"sql":"SELECT 1"}"#).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn safe_run_sql_rejects_non_select_for_admin() {
        let resp = do_post_as_admin("/tools/safe_run_sql", r#"{"sql":"DROP TABLE users"}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert_eq!(json["status"].as_str(), Some("rejected"));
    }

    #[tokio::test]
    async fn safe_run_sql_accepts_select_for_admin() {
        let resp =
            do_post_as_admin("/tools/safe_run_sql", r#"{"sql":"SELECT 42 AS answer"}"#).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"].as_str(), Some("ok"));
    }

    #[tokio::test]
    async fn propose_migration_generates_ctas_sql() {
        let resp = do_post_as_admin(
            "/tools/propose_migration",
            r#"{"target_table":"fact_sales"}"#,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert!(
            json["ctas_sql"]
                .as_str()
                .unwrap_or("")
                .contains("fact_sales_mart")
        );
    }

    #[tokio::test]
    async fn propose_migration_rejects_invalid_table_name() {
        let resp = do_post_as_admin("/tools/propose_migration", r#"{"target_table":"!!!"}"#).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn safe_run_sql_with_jwt_denies_table_without_object_grant() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                9,
                "analyst",
                "ANALYST",
                "default",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/safe_run_sql",
            r#"{"sql":"SELECT * FROM secret_orders"}"#,
            &token,
            test_policy(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn safe_run_sql_with_jwt_denies_comment_adjacent_table_without_object_grant() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                10,
                "analyst",
                "ANALYST",
                "default",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/safe_run_sql",
            r#"{"sql":"SELECT * FROM secret_orders-- qa comment\n"}"#,
            &token,
            test_policy(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn safe_run_sql_with_jwt_denies_invalid_object_identifier_parse_failure() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                10,
                "analyst",
                "ANALYST",
                "default",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/safe_run_sql",
            r#"{"sql":"SELECT * FROM !!secret_orders"}"#,
            &token,
            test_policy(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn safe_run_sql_with_jwt_catalog_grant_reaches_execution() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                10,
                "analyst",
                "ANALYST",
                "default",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        let policy = test_policy();
        policy
            .grant_privilege(
                "ANALYST",
                Privilege::Select,
                ObjectType::Table,
                "secret_orders",
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/safe_run_sql",
            r#"{"sql":"SELECT * FROM secret_orders"}"#,
            &token,
            policy,
        )
        .await;
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn propose_schema_with_jwt_denies_requested_table_without_object_grant() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                11,
                "analyst",
                "ANALYST",
                "default",
                vec!["table.select".to_string()],
                1,
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/propose_schema",
            r#"{"table_name":"secret_orders"}"#,
            &token,
            test_policy(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn propose_schema_with_jwt_catalog_grant_allows_requested_table_metadata() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                12,
                "analyst",
                "ANALYST",
                "default",
                vec!["table.select".to_string()],
                1,
            )
            .unwrap();
        let policy = test_policy();
        policy
            .grant_privilege(
                "ANALYST",
                Privilege::Select,
                ObjectType::Table,
                "secret_orders",
            )
            .unwrap();
        let resp = do_post_with_bearer_and_policy(
            "/tools/propose_schema",
            r#"{"table_name":"secret_orders"}"#,
            &token,
            policy,
        )
        .await;
        assert_ne!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn propose_migration_with_jwt_requires_source_select_and_target_create_grants() {
        let jwt = opensnow_auth::JwtManager::new(b"mcp-jwt-secret");
        let token = jwt
            .generate_token_with_scopes(
                13,
                "analyst",
                "ANALYST",
                "default",
                vec!["table.create".to_string()],
                1,
            )
            .unwrap();
        let select_only = test_policy();
        select_only
            .grant_privilege(
                "ANALYST",
                Privilege::Select,
                ObjectType::Table,
                "fact_sales",
            )
            .unwrap();
        let denied = do_post_with_bearer_and_policy(
            "/tools/propose_migration",
            r#"{"target_table":"fact_sales"}"#,
            &token,
            select_only,
        )
        .await;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let allowed_policy = test_policy();
        allowed_policy
            .grant_privilege(
                "ANALYST",
                Privilege::Select,
                ObjectType::Table,
                "fact_sales",
            )
            .unwrap();
        allowed_policy
            .grant_privilege(
                "ANALYST",
                Privilege::Create,
                ObjectType::Table,
                "fact_sales_mart",
            )
            .unwrap();
        let allowed = do_post_with_bearer_and_policy(
            "/tools/propose_migration",
            r#"{"target_table":"fact_sales"}"#,
            &token,
            allowed_policy,
        )
        .await;
        assert_eq!(allowed.status(), StatusCode::OK);
    }
}
