use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use opensnow_core::{EngineHandle, OpenSnowEngine};
use opensnow_distributed::{
    DistributedExecutor, LocalWorkerExecutor, PartitionStrategy, WorkerExecutor,
};
use serde_json::{Value, json};

use crate::ingest_buffer::{IngestBuffer, SharedBuffer};
use crate::metrics::{
    dec_active_queries, dec_warehouse_pending, inc_active_queries, inc_warehouse_pending,
    metrics_handler, record_query,
};
use crate::tenant::{TenantId, tenant_middleware};

/// `EngineHandle` is Send + Sync — safe to use as axum Router state.
pub type AppState = EngineHandle;

pub(crate) const APP_UI: &str = include_str!("../static/app.html");
pub(crate) const PUBLIC_TEST_UI: &str = include_str!("../static/index.html");
// Vendored Vega-Lite (BSD) so the Build tab's charts render with no external CDN.
const VEGA_JS: &str = include_str!("../static/vendor/vega.min.js");
const VEGA_LITE_JS: &str = include_str!("../static/vendor/vega-lite.min.js");
const VEGA_EMBED_JS: &str = include_str!("../static/vendor/vega-embed.min.js");
// Social-share (OpenGraph) preview image for opensnow.ebdsweden.com.
const OG_IMAGE: &[u8] = include_bytes!("../static/og-image.png");
const DEPLOYMENT_DOC: &str = include_str!("../../../docs/DEPLOYMENT.md");
const SQL_COMPATIBILITY_DOC_BODY: &str = include_str!("../../../docs/SQL_COMPATIBILITY.md");
const PUBLIC_TEST_PATH_DOC: &str = include_str!("../../../docs/PUBLIC_TEST_PATH.md");
const MAX_DEMO_QUERY_BYTES: usize = 64 * 1024;
const DEFAULT_QUERY_TIMEOUT_SECS: u64 = 30;
const MAX_QUERY_TIMEOUT_SECS: u64 = 300;
const SQL_COMPATIBILITY_DOC: &str = "docs/SQL_COMPATIBILITY.md";

fn first_scalar_usize(batches: &[arrow::record_batch::RecordBatch]) -> Option<usize> {
    let batch = batches.first()?;
    if batch.num_rows() == 0 || batch.num_columns() == 0 {
        return None;
    }
    arrow::util::display::array_value_to_string(batch.column(0), 0)
        .ok()?
        .parse()
        .ok()
}

/// Create a router by spawning a new engine worker from the given `OpenSnowEngine`.
pub fn create_router(engine: OpenSnowEngine) -> Router {
    let warehouse = engine.warehouse_path().to_string();
    let handle = EngineHandle::spawn(engine);
    let buffer = IngestBuffer::shared(warehouse);
    crate::ingest_buffer::spawn_compactor(buffer.clone(), handle.clone());
    create_router_with_auth_and_buffer(handle, None, buffer)
}

/// Create a router from an already-spawned `EngineHandle`. No auth.
/// Tests use this; production uses `create_router_with_auth`.
pub fn create_router_with_handle(handle: EngineHandle) -> Router {
    // Tests don't need real ingest persistence — point the buffer at a
    // temp dir. Callers that want a real path should use
    // `create_router_with_auth_and_buffer`.
    let buffer = IngestBuffer::shared(std::env::temp_dir().join("opensnow-test-ingest"));
    create_router_with_auth_and_buffer(handle, None, buffer)
}

/// Create a router with optional JWT auth.
///
/// When `auth` is `Some`, `/api/v1/query`, `/api/v1/ingest` and
/// `/api/v1/distributed_query` are protected by the
/// [`crate::auth::jwt_required`] middleware and a `POST /auth/token`
/// endpoint is exposed for OAuth2 client_credentials. `/health`,
/// `/metrics`, and `/` (the query UI) remain public.
pub fn create_router_with_auth(
    handle: EngineHandle,
    auth: Option<crate::auth::AuthState>,
) -> Router {
    let buffer = IngestBuffer::shared(std::env::temp_dir().join("opensnow-ingest"));
    create_router_with_auth_and_buffer(handle, auth, buffer)
}

/// Full-control router builder. Used by [`OpenSnowServer`] which owns the
/// ingest buffer and starts the compactor explicitly.
pub fn create_router_with_auth_and_buffer(
    handle: EngineHandle,
    auth: Option<crate::auth::AuthState>,
    buffer: SharedBuffer,
) -> Router {
    let sso_manager = crate::admin::default_sso_manager();
    let mut admin = crate::admin::admin_router(sso_manager.clone());
    let mut dbt = crate::dbt::router(handle.clone());

    // Public engine routes that don't need auth. Tenant reads stay public for
    // local/demo discovery; tenant mutation is split below so auth-enabled
    // deployments can require platform-admin policy scope.
    let public_engine: Router = Router::new()
        .route("/api/v1/status", get(status))
        .route("/api/v1/tenants", get(list_tenants))
        .with_state(handle.clone());
    let mut tenant_write_routes: Router = Router::new()
        .route("/api/v1/tenants", axum::routing::post(create_tenant))
        .with_state(handle.clone());

    // Streaming ingest endpoints share the buffer.
    let mut ingest_batch_routes: Router = Router::new()
        .route("/api/v1/ingest/batch", axum::routing::post(ingest_batch))
        .with_state(buffer.clone());
    let mut ingest_status_routes: Router = Router::new()
        .route("/api/v1/ingest/status", get(ingest_status))
        .with_state(buffer);

    // Routes that the user may elect to protect with JWT. In auth-disabled
    // local demo mode they remain open so first-run testers can run a query
    // without bootstrapping credentials. When auth is enabled below, each route
    // receives JWT validation plus a narrower route authorization guard.
    let mut query_routes: Router = Router::new()
        .route("/api/v1/query", axum::routing::post(execute_query))
        .with_state(handle.clone());
    let mut distributed_query_routes: Router = Router::new()
        .route(
            "/api/v1/distributed_query",
            axum::routing::post(distributed_query),
        )
        .with_state(handle.clone());
    let mut ingest_write_routes: Router = Router::new()
        .route("/api/v1/ingest", axum::routing::post(ingest))
        .route("/api/v1/demo/load", axum::routing::post(load_demo_data))
        .with_state(handle.clone());
    // Registering an external Parquet table (local or s3://, gs://, az://) is a
    // privileged operation — admin scope when auth is enabled.
    let mut table_admin_routes: Router = Router::new()
        .route(
            "/api/v1/tables/register",
            axum::routing::post(register_table),
        )
        .route(
            "/api/v1/export/postgres",
            axum::routing::post(export_postgres),
        )
        .with_state(handle);

    // Pipeline/lineage view (read-only, public) + the run trigger (admin-scoped
    // when auth is enabled).
    let pipeline = crate::pipeline::build();
    let pipeline_public = pipeline.public;
    let mut pipeline_admin = pipeline.admin;

    let mut router = Router::new()
        .route("/", get(query_ui))
        .route("/public-test", get(public_test_ui))
        .route("/docs/DEPLOYMENT.md", get(deployment_doc))
        .route("/docs/SQL_COMPATIBILITY.md", get(sql_compatibility_doc))
        .route("/docs/PUBLIC_TEST_PATH.md", get(public_test_path_doc))
        // Self-hosted Vega-Lite (no CDN) for the native Build tab.
        .route("/vendor/vega.min.js", get(|| async { js_asset(VEGA_JS) }))
        .route(
            "/vendor/vega-lite.min.js",
            get(|| async { js_asset(VEGA_LITE_JS) }),
        )
        .route(
            "/vendor/vega-embed.min.js",
            get(|| async { js_asset(VEGA_EMBED_JS) }),
        )
        .route(
            "/og-image.png",
            get(|| async { ([(header::CONTENT_TYPE, "image/png")], OG_IMAGE).into_response() }),
        )
        .route("/health", get(health))
        .route("/metrics", get(metrics_handler));

    if let Some(auth_state) = auth.clone() {
        // route_layer keeps the middleware scoped to just the protected
        // routes so /health and /metrics remain reachable without a token.
        let auth_layer =
            axum::middleware::from_fn_with_state(auth_state.clone(), crate::auth::jwt_required);
        query_routes = query_routes
            .route_layer(axum::middleware::from_fn(crate::auth::require_query_scope))
            .route_layer(auth_layer.clone());
        distributed_query_routes = distributed_query_routes
            .route_layer(axum::middleware::from_fn(crate::auth::require_query_scope))
            .route_layer(auth_layer.clone());
        ingest_write_routes = ingest_write_routes
            .route_layer(axum::middleware::from_fn(
                crate::auth::require_ingest_write_scope,
            ))
            .route_layer(auth_layer.clone());
        ingest_batch_routes = ingest_batch_routes
            .route_layer(axum::middleware::from_fn(
                crate::auth::require_ingest_write_scope,
            ))
            .route_layer(auth_layer.clone());
        ingest_status_routes = ingest_status_routes
            .route_layer(axum::middleware::from_fn(
                crate::auth::require_ingest_read_scope,
            ))
            .route_layer(auth_layer.clone());
        tenant_write_routes = tenant_write_routes
            .route_layer(axum::middleware::from_fn(crate::auth::require_admin_scope))
            .route_layer(auth_layer.clone());
        table_admin_routes = table_admin_routes
            .route_layer(axum::middleware::from_fn(crate::auth::require_admin_scope))
            .route_layer(auth_layer.clone());
        pipeline_admin = pipeline_admin
            .route_layer(axum::middleware::from_fn(crate::auth::require_admin_scope))
            .route_layer(auth_layer.clone());
        admin = admin
            .route_layer(axum::middleware::from_fn(crate::auth::require_admin_scope))
            .route_layer(auth_layer.clone());
        dbt = dbt
            .route_layer(axum::middleware::from_fn(crate::auth::require_query_scope))
            .route_layer(auth_layer);
        router = router.merge(crate::auth::auth_router(auth_state));
    }

    router
        .merge(public_engine)
        .merge(query_routes)
        .merge(distributed_query_routes)
        .merge(ingest_write_routes)
        .merge(tenant_write_routes)
        .merge(table_admin_routes)
        .merge(admin)
        .merge(crate::admin::auth_login_router(sso_manager, auth.clone()))
        .merge(dbt)
        // Pipeline/lineage view (public read-only) + admin-scoped run trigger.
        .merge(pipeline_public)
        .merge(pipeline_admin)
        // Saved chart/dashboard specs for the native Build tab (file-backed).
        // Reads stay public; mutations inherit auth/RBAC when auth is enabled.
        .merge(crate::charts::router(auth))
        .merge(ingest_batch_routes)
        .merge(ingest_status_routes)
        // Tenant resolution runs for every request — public and protected.
        // Handlers that care about tenancy extract `TenantId` from the request.
        .layer(axum::middleware::from_fn(tenant_middleware))
}

async fn query_ui() -> Html<String> {
    ui_asset("app.html", APP_UI)
}

async fn public_test_ui() -> Html<String> {
    ui_asset("index.html", PUBLIC_TEST_UI)
}

/// Serve a UI asset from `OPENSNOW_UI_DIR` when set (so HTML can be hot-swapped
/// via a mounted volume without rebuilding the image), else the embedded copy.
pub(crate) fn ui_asset(file: &str, embedded: &'static str) -> Html<String> {
    if let Ok(dir) = std::env::var("OPENSNOW_UI_DIR") {
        if let Ok(body) = std::fs::read_to_string(std::path::Path::new(&dir).join(file)) {
            return Html(body);
        }
    }
    Html(embedded.to_string())
}

fn js_asset(body: &'static str) -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

fn markdown_response(body: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
        body,
    )
        .into_response()
}

async fn deployment_doc() -> Response {
    markdown_response(DEPLOYMENT_DOC)
}

async fn sql_compatibility_doc() -> Response {
    markdown_response(SQL_COMPATIBILITY_DOC_BODY)
}

async fn public_test_path_doc() -> Response {
    markdown_response(PUBLIC_TEST_PATH_DOC)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "engine": "opensnow" }))
}

async fn status(State(_handle): State<AppState>) -> Json<Value> {
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "engine": "Apache DataFusion",
        "status": "running"
    }))
}

const DEMO_TABLE: &str = "opensnow_demo_orders";
const DEMO_SAMPLE_QUERY: &str = "SELECT region, SUM(amount) AS revenue FROM opensnow_demo_orders GROUP BY region ORDER BY revenue DESC";
const LOAD_DEMO_SQL: &str = "CREATE TABLE opensnow_demo_orders AS SELECT * FROM (VALUES\n    (1, 'Nordics', '2026-05-01', 1250.50, 'starter'),\n    (2, 'DACH', '2026-05-02', 3420.00, 'enterprise'),\n    (3, 'Benelux', '2026-05-03', 980.25, 'starter'),\n    (4, 'Nordics', '2026-05-04', 2875.00, 'enterprise'),\n    (5, 'UKI', '2026-05-05', 1995.75, 'growth'),\n    (6, 'DACH', '2026-05-06', 1750.00, 'growth')\n) AS t(order_id, region, order_date, amount, plan)";

#[tracing::instrument(name = "rest.demo_load", skip(handle))]
async fn load_demo_data(State(handle): State<AppState>) -> Json<Value> {
    if let Ok(existing) = handle
        .execute_sql(&format!("SELECT COUNT(*) AS rows FROM {DEMO_TABLE}"))
        .await
    {
        let rows = first_scalar_usize(&existing).unwrap_or_default();
        return Json(json!({
            "status": "ok",
            "table": DEMO_TABLE,
            "already_loaded": true,
            "rows_ingested": 0,
            "rows_checked": rows,
            "sample_query": DEMO_SAMPLE_QUERY,
            "next_step": "Demo data is already loaded. Continue to queries by running the sample query from the browser picker or POST it to /api/v1/query.",
        }));
    }

    match handle.execute_sql(LOAD_DEMO_SQL).await {
        Ok(_) => Json(json!({
            "status": "ok",
            "table": DEMO_TABLE,
            "already_loaded": false,
            "rows_ingested": 6,
            "sample_query": DEMO_SAMPLE_QUERY,
            "next_step": "Run the sample query from the browser picker or POST it to /api/v1/query.",
        })),
        Err(e) => Json(json!({
            "status": "error",
            "message": format!("failed to load demo data: {e}"),
            "next_step": "Check server logs and docs/DEPLOYMENT.md, then retry Load demo data.",
        })),
    }
}

#[derive(Debug, serde::Deserialize)]
struct RegisterTableRequest {
    /// Unqualified, safe table name to expose the data under.
    name: String,
    /// Parquet location: a local path or an object-store URI
    /// (`s3://bucket/key.parquet`, `gs://…`, `az://…`). Directories of Parquet
    /// are also accepted by the underlying listing table.
    uri: String,
}

/// Allow letters, digits and underscore; must start with a letter/underscore.
fn is_safe_table_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Accept object-store URIs and ordinary local paths; reject anything that
/// looks like a shell/scheme injection surface.
fn is_supported_parquet_uri(uri: &str) -> bool {
    let ok_scheme = ["s3://", "gs://", "gcs://", "az://", "abfs://", "file://"]
        .iter()
        .any(|s| uri.starts_with(s));
    let looks_local = uri.starts_with('/') || uri.starts_with("./") || uri.starts_with("../");
    (ok_scheme || looks_local) && !uri.contains('\n') && uri.len() <= 2048
}

/// Register an external Parquet file/dir (local or object-store URI) as a
/// queryable table. This is the supported path for querying S3/GCS/Azure data
/// directly — once registered, the table is visible to the standard
/// (demo-safe) `SELECT` surface. Protected by the admin scope when auth is on.
#[tracing::instrument(name = "rest.register_table", skip(handle))]
async fn register_table(
    State(handle): State<AppState>,
    Json(req): Json<RegisterTableRequest>,
) -> Response {
    if !is_safe_table_name(&req.name) {
        return error_json(
            StatusCode::BAD_REQUEST,
            "invalid table name: use letters, digits and underscores, starting with a letter",
        );
    }
    if !is_supported_parquet_uri(&req.uri) {
        return error_json(
            StatusCode::BAD_REQUEST,
            "unsupported uri: expected a local path or an s3://, gs://, az:// or file:// Parquet location",
        );
    }
    match handle.register_parquet(&req.name, &req.uri).await {
        Ok(()) => Json(json!({
            "status": "ok",
            "table": req.name,
            "uri": req.uri,
            "next_step": format!("SELECT * FROM {} LIMIT 10 via /api/v1/query", req.name),
        }))
        .into_response(),
        Err(e) => error_json(
            StatusCode::BAD_GATEWAY,
            &format!("failed to register table from {}: {e}", req.uri),
        ),
    }
}

fn error_json(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({ "status": "error", "message": message }))).into_response()
}

/// Serialize a full result set (every batch/partition) to newline-delimited
/// JSON. Writing only the first batch would silently truncate multi-batch
/// results — e.g. partitioned aggregate/window output — while the reported row
/// count still reflected the full total.
fn batches_to_ndjson(batches: &[arrow::array::RecordBatch]) -> String {
    if batches.is_empty() {
        return String::new();
    }
    let refs: Vec<&arrow::array::RecordBatch> = batches.iter().collect();
    let buf = Vec::new();
    let mut writer = arrow::json::LineDelimitedWriter::new(buf);
    writer.write_batches(&refs).ok();
    writer.finish().ok();
    String::from_utf8(writer.into_inner()).unwrap_or_default()
}

#[derive(serde::Deserialize)]
struct ExportPostgresRequest {
    /// Query to run in OpenSnow; its result set is written to Postgres.
    sql: String,
    /// Target Postgres DSN, e.g. `postgres://user:pass@host:5432/db`.
    dsn: String,
    /// Target table name (safe identifier).
    table: String,
    /// Target schema (defaults to `public`).
    #[serde(default = "default_export_schema")]
    schema: String,
    /// `replace` (default) or `append`.
    #[serde(default)]
    mode: Option<String>,
}

fn default_export_schema() -> String {
    "public".to_string()
}

/// Run a query in OpenSnow and load the result into an external Postgres table
/// — the "serving layer" sink. Admin-scoped when auth is enabled.
#[tracing::instrument(name = "rest.export_postgres", skip(handle, req))]
async fn export_postgres(
    State(handle): State<AppState>,
    Json(req): Json<ExportPostgresRequest>,
) -> Response {
    let mode = match crate::pg_sink::WriteMode::parse(req.mode.as_deref().unwrap_or("replace")) {
        Ok(m) => m,
        Err(e) => return error_json(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    let sql = match validate_export_postgres_sql(&req.sql) {
        Ok(s) => s,
        Err(e) => return error_json(StatusCode::BAD_REQUEST, &e),
    };
    match crate::pg_sink::export_to_postgres(&handle, &sql, &req.dsn, &req.schema, &req.table, mode)
        .await
    {
        Ok(rows) => Json(json!({
            "status": "ok",
            "rows_written": rows,
            "target": format!("{}.{}", req.schema, req.table),
        }))
        .into_response(),
        Err(e) => error_json(StatusCode::BAD_GATEWAY, &format!("export failed: {e}")),
    }
}

#[derive(serde::Deserialize)]
struct QueryRequest {
    sql: String,
    #[serde(default)]
    warehouse: Option<String>,
}

fn query_timeout_duration() -> Duration {
    let secs = std::env::var("OPENSNOW_QUERY_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_QUERY_TIMEOUT_SECS)
        .min(MAX_QUERY_TIMEOUT_SECS);
    Duration::from_secs(secs)
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

fn starts_with_keyword_sequence(sql: &str, expected: &[&str]) -> bool {
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    tokens.len() >= expected.len()
        && tokens
            .iter()
            .take(expected.len())
            .zip(expected.iter())
            .all(|(actual, expected)| actual.eq_ignore_ascii_case(expected))
}

fn find_as_clause(sql: &str) -> Option<&str> {
    let mut quote: Option<char> = None;
    let mut chars = sql.char_indices().peekable();

    while let Some((idx, ch)) = chars.next() {
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
                    return Some(sql[as_end..].trim_start());
                }
            }
            _ => {}
        }
    }

    None
}

fn is_valid_demo_relation_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

fn relation_token_after_create_prefix<'sql>(
    tokens: &[&'sql str],
    prefix_len: usize,
) -> Option<&'sql str> {
    let target = *tokens.get(prefix_len)?;
    if target.eq_ignore_ascii_case("IF")
        && tokens
            .get(prefix_len + 1)
            .is_some_and(|token| token.eq_ignore_ascii_case("NOT"))
        && tokens
            .get(prefix_len + 2)
            .is_some_and(|token| token.eq_ignore_ascii_case("EXISTS"))
    {
        return tokens.get(prefix_len + 3).copied();
    }
    Some(target)
}

fn create_relation_target(sql: &str) -> Option<&str> {
    let tokens: Vec<&str> = sql.split_whitespace().collect();
    match tokens.as_slice() {
        [create, table, ..]
            if create.eq_ignore_ascii_case("CREATE") && table.eq_ignore_ascii_case("TABLE") =>
        {
            relation_token_after_create_prefix(&tokens, 2)
        }
        [create, materialized, view, ..]
            if create.eq_ignore_ascii_case("CREATE")
                && materialized.eq_ignore_ascii_case("MATERIALIZED")
                && view.eq_ignore_ascii_case("VIEW") =>
        {
            relation_token_after_create_prefix(&tokens, 3)
        }
        _ => None,
    }
}

fn create_materialization_query(sql: &str) -> Option<&str> {
    let is_materializing_create = starts_with_keyword_sequence(sql, &["CREATE", "TABLE"])
        || starts_with_keyword_sequence(sql, &["CREATE", "MATERIALIZED", "VIEW"]);
    if !is_materializing_create {
        return None;
    }
    find_as_clause(sql)
}

fn validate_demo_materialization_query(sql: &str) -> Result<(), String> {
    let Some(query_sql) = create_materialization_query(sql) else {
        return Ok(());
    };
    match first_keyword(query_sql).as_str() {
        "SELECT" | "WITH" => Ok(()),
        keyword => Err(format!(
            "Unsupported SQL statement for the external demo: CREATE materialization query must start with SELECT or WITH, got {keyword}. See {SQL_COMPATIBILITY_DOC}."
        )),
    }
}

fn is_supported_demo_statement(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    let first = first_keyword(sql);
    match first.as_str() {
        "SELECT" | "WITH" | "EXPLAIN" | "SHOW" | "DESCRIBE" | "DESC" => true,
        "CREATE" => {
            (upper.starts_with("CREATE TABLE") && contains_keyword(sql, "AS"))
                || upper.starts_with("CREATE MATERIALIZED VIEW")
                || upper.starts_with("CREATE WAREHOUSE")
        }
        "REFRESH" => upper.starts_with("REFRESH MATERIALIZED VIEW"),
        "ALTER" => upper.starts_with("ALTER WAREHOUSE"),
        "USE" => upper.starts_with("USE WAREHOUSE"),
        _ => false,
    }
}

fn validate_demo_sql(sql: &str) -> Result<String, String> {
    if sql.trim().is_empty() {
        return Err("SQL must not be empty. Try: SELECT 1 AS smoke".to_string());
    }
    if sql.len() > MAX_DEMO_QUERY_BYTES {
        return Err(format!(
            "SQL text is too large for the external demo ({} bytes max). Use a smaller statement or load data through documented ingest paths.",
            MAX_DEMO_QUERY_BYTES
        ));
    }

    let statements = split_sql_statements(sql);
    if statements.len() != 1 {
        return Err(format!(
            "OpenSnow demo accepts one SQL statement per request; received {}. Split multi-step workflows into separate requests. See {SQL_COMPATIBILITY_DOC}.",
            statements.len()
        ));
    }

    let statement = statements.into_iter().next().unwrap();
    if let Some(target) = create_relation_target(&statement) {
        if !is_valid_demo_relation_name(target) {
            return Err(format!(
                "invalid demo table identifier: {target}. Use an unquoted table name with letters, numbers, and underscores only. See {SQL_COMPATIBILITY_DOC}."
            ));
        }
    }
    validate_demo_materialization_query(&statement)?;

    if !is_supported_demo_statement(&statement) {
        let keyword = first_keyword(&statement);
        return Err(format!(
            "Unsupported SQL statement for the external demo: {keyword}. Supported demo statements include SELECT/WITH/EXPLAIN, SHOW, DESCRIBE, CREATE TABLE AS SELECT with safe identifiers, CREATE/REFRESH MATERIALIZED VIEW, and warehouse SHOW/CREATE/ALTER/USE. Use /api/v1/ingest for demo loads. See {SQL_COMPATIBILITY_DOC}."
        ));
    }

    Ok(statement)
}

fn sql_keywords_outside_literals(sql: &str) -> Vec<String> {
    let mut keywords = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    let mut quote: Option<char> = None;
    let mut line_comment = false;
    let mut block_comment = false;

    while let Some(ch) = chars.next() {
        if line_comment {
            if ch == '\n' {
                line_comment = false;
            }
            continue;
        }
        if block_comment {
            if ch == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_comment = false;
            }
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                if chars.peek() == Some(&q) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }

        if ch == '-' && chars.peek() == Some(&'-') {
            chars.next();
            line_comment = true;
            if !current.is_empty() {
                keywords.push(std::mem::take(&mut current).to_ascii_uppercase());
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            block_comment = true;
            if !current.is_empty() {
                keywords.push(std::mem::take(&mut current).to_ascii_uppercase());
            }
            continue;
        }

        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                if !current.is_empty() {
                    keywords.push(std::mem::take(&mut current).to_ascii_uppercase());
                }
            }
            _ if ch.is_ascii_alphanumeric() || ch == '_' => current.push(ch),
            _ => {
                if !current.is_empty() {
                    keywords.push(std::mem::take(&mut current).to_ascii_uppercase());
                }
            }
        }
    }
    if !current.is_empty() {
        keywords.push(current.to_ascii_uppercase());
    }
    keywords
}

fn is_export_postgres_mutating_keyword(keyword: &str) -> bool {
    matches!(
        keyword,
        "CREATE"
            | "ALTER"
            | "REFRESH"
            | "USE"
            | "DROP"
            | "COPY"
            | "INSERT"
            | "UPDATE"
            | "DELETE"
            | "MERGE"
            | "TRUNCATE"
            | "GRANT"
            | "REVOKE"
            | "CALL"
            | "SET"
            | "RESET"
            | "UNLOAD"
            | "VACUUM"
            | "OPTIMIZE"
    )
}

fn validate_export_postgres_sql(sql: &str) -> Result<String, String> {
    if sql.trim().is_empty() {
        return Err("SQL must not be empty. Try: SELECT 1 AS smoke".to_string());
    }
    if sql.len() > MAX_DEMO_QUERY_BYTES {
        return Err(format!(
            "SQL text is too large for Postgres export ({} bytes max). Use a smaller read-only query.",
            MAX_DEMO_QUERY_BYTES
        ));
    }

    let statements = split_sql_statements(sql);
    if statements.len() != 1 {
        return Err(format!(
            "Postgres export accepts one read-only result-producing SQL statement per request; received {}.",
            statements.len()
        ));
    }

    let statement = statements.into_iter().next().unwrap();
    match first_keyword(&statement).as_str() {
        "SELECT" | "WITH" | "EXPLAIN" => {}
        keyword => {
            return Err(format!(
                "Postgres export accepts only read-only result-producing SQL (SELECT/WITH/EXPLAIN); got {keyword}."
            ));
        }
    }

    if let Some(keyword) = sql_keywords_outside_literals(&statement)
        .iter()
        .find(|keyword| is_export_postgres_mutating_keyword(keyword))
    {
        return Err(format!(
            "Postgres export accepts only read-only result-producing SQL; rejected state-changing keyword {keyword}."
        ));
    }

    Ok(statement)
}

fn user_facing_query_error(error: &anyhow::Error) -> String {
    let raw = error.to_string();
    if raw.contains("This feature is not implemented")
        || raw.contains("Unsupported logical plan")
        || raw.contains("not implemented")
    {
        format!(
            "OpenSnow does not support this SQL shape yet. {raw}. See {SQL_COMPATIBILITY_DOC} for supported SQL and known limits."
        )
    } else {
        format!("OpenSnow could not execute this SQL: {raw}")
    }
}

#[derive(serde::Deserialize)]
struct IngestRequest {
    table: String,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
    #[serde(default)]
    replace: bool,
}

fn is_valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
}

/// Render a JSON scalar value as a SQL literal. We only support the primitive
/// types needed for the test ingest path: numbers, booleans, strings, null.
fn json_to_sql_literal(v: &Value) -> Result<String, String> {
    match v {
        Value::Null => Ok("NULL".to_string()),
        Value::Bool(b) => Ok(if *b {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        }),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(format!("'{}'", s.replace('\'', "''"))),
        _ => Err("only scalar values are supported in ingest rows".to_string()),
    }
}

/// Lightweight ingest endpoint used for tests and small ad-hoc loads.
/// Builds a `CREATE TABLE <name> AS SELECT * FROM (VALUES ...) AS t(<columns>)`
/// statement and runs it via the engine. Not intended for high-volume loads.
#[tracing::instrument(
    name = "rest.ingest",
    skip(handle, req),
    fields(table = %req.table, rows = req.rows.len())
)]
async fn ingest(State(handle): State<AppState>, Json(req): Json<IngestRequest>) -> Json<Value> {
    if !is_valid_ident(&req.table) {
        return Json(json!({ "status": "error", "message": "invalid table name" }));
    }
    if req.columns.is_empty() {
        return Json(json!({ "status": "error", "message": "columns must be non-empty" }));
    }
    for c in &req.columns {
        if !is_valid_ident(c) {
            return Json(json!({ "status": "error", "message": format!("invalid column: {c}") }));
        }
    }
    if req.rows.is_empty() {
        return Json(json!({ "status": "error", "message": "rows must be non-empty" }));
    }
    if req.rows.iter().any(|r| r.len() != req.columns.len()) {
        return Json(json!({ "status": "error", "message": "row length mismatch" }));
    }

    let mut row_literals = Vec::with_capacity(req.rows.len());
    for row in &req.rows {
        let mut cells = Vec::with_capacity(row.len());
        for cell in row {
            match json_to_sql_literal(cell) {
                Ok(lit) => cells.push(lit),
                Err(e) => return Json(json!({ "status": "error", "message": e })),
            }
        }
        row_literals.push(format!("({})", cells.join(", ")));
    }

    if req.replace {
        let drop_sql = format!("DROP TABLE IF EXISTS {}", req.table);
        let _ = handle.execute_sql(&drop_sql).await;
    }

    let column_list = req.columns.join(", ");
    let values_block = row_literals.join(", ");
    let create_sql = format!(
        "CREATE TABLE {} AS SELECT * FROM (VALUES {}) AS t({})",
        req.table, values_block, column_list
    );

    match handle.execute_sql(&create_sql).await {
        Ok(_) => Json(json!({
            "status": "ok",
            "table": req.table,
            "rows_ingested": req.rows.len(),
        })),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

#[tracing::instrument(
    name = "rest.query",
    skip(handle, req, tenant),
    fields(
        warehouse = %req.warehouse.as_deref().unwrap_or("default"),
        tenant = %tenant.as_str(),
        sql_len = req.sql.len(),
    )
)]
async fn execute_query(
    State(handle): State<AppState>,
    tenant: TenantId,
    Json(req): Json<QueryRequest>,
) -> Json<Value> {
    let warehouse = req.warehouse.as_deref().unwrap_or("default");
    let start = Instant::now();
    let sql = match validate_demo_sql(&req.sql) {
        Ok(sql) => sql,
        Err(message) => return Json(json!({ "status": "error", "message": message })),
    };

    inc_active_queries();
    inc_warehouse_pending(warehouse);

    // Run the engine call inside its own span so OTel can show the
    // parse → plan → execute boundary against the outer rest.query span.
    // `Instrument` is required here — `Span::enter` would hold the guard
    // across the `.await` and break in async contexts.
    let result = {
        use tokio::time::timeout;
        use tracing::Instrument;
        let span = tracing::info_span!("engine.execute_sql", sql_len = sql.len());
        timeout(
            query_timeout_duration(),
            handle.execute_sql(&sql).instrument(span),
        )
        .await
    };

    dec_warehouse_pending(warehouse);
    dec_active_queries();

    match result {
        Ok(Ok(batches)) => {
            let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            let duration = start.elapsed();
            record_query(duration, "success", total_rows as u64);
            let duration_ms = duration.as_millis() as i64;
            handle
                .record_query_history_for_tenant(
                    tenant.as_str(),
                    warehouse,
                    &sql,
                    duration_ms,
                    total_rows as i64,
                    "success",
                )
                .await;

            let rows = batches_to_ndjson(&batches);

            Json(json!({ "status": "ok", "rows": total_rows, "data": rows }))
        }
        Ok(Err(e)) => {
            let duration = start.elapsed();
            record_query(duration, "error", 0);
            let duration_ms = duration.as_millis() as i64;
            handle
                .record_query_history_for_tenant(
                    tenant.as_str(),
                    warehouse,
                    &sql,
                    duration_ms,
                    0,
                    "error",
                )
                .await;
            Json(json!({ "status": "error", "message": user_facing_query_error(&e) }))
        }
        Err(_) => {
            let duration = start.elapsed();
            record_query(duration, "timeout", 0);
            let duration_ms = duration.as_millis() as i64;
            handle
                .record_query_history_for_tenant(
                    tenant.as_str(),
                    warehouse,
                    &sql,
                    duration_ms,
                    0,
                    "timeout",
                )
                .await;
            Json(json!({
                "status": "error",
                "message": format!(
                    "Query exceeded the external-demo timeout of {} seconds. Narrow the scan, add LIMIT, or raise OPENSNOW_QUERY_TIMEOUT_SECS for trusted local testing (max {} seconds).",
                    query_timeout_duration().as_secs(),
                    MAX_QUERY_TIMEOUT_SECS
                )
            }))
        }
    }
}

// ── Tenant management ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CreateTenantRequest {
    name: String,
}

async fn list_tenants(State(handle): State<AppState>) -> Json<Value> {
    match handle.list_tenants().await {
        Ok(tenants) => Json(json!({ "status": "ok", "tenants": tenants })),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

async fn create_tenant(
    State(handle): State<AppState>,
    Json(req): Json<CreateTenantRequest>,
) -> Json<Value> {
    let name = req.name.trim();
    if name.is_empty() {
        return Json(json!({ "status": "error", "message": "name must not be empty" }));
    }
    match handle.create_tenant(name).await {
        Ok(tenant) => Json(json!({ "status": "ok", "tenant": tenant })),
        Err(e) => Json(json!({ "status": "error", "message": e.to_string() })),
    }
}

// ── Streaming ingest ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct IngestBatchRequest {
    table: String,
    rows: Vec<Value>,
}

async fn ingest_batch(
    State(buffer): State<SharedBuffer>,
    tenant: TenantId,
    Json(req): Json<IngestBatchRequest>,
) -> Json<Value> {
    if !is_valid_ident(&req.table) {
        return Json(json!({ "status": "error", "message": "invalid table name" }));
    }
    if req.rows.is_empty() {
        return Json(json!({ "status": "error", "message": "rows must be non-empty" }));
    }
    if req.rows.iter().any(|r| !r.is_object()) {
        return Json(json!({ "status": "error", "message": "every row must be a JSON object" }));
    }
    let received = req.rows.len();
    let buffered = buffer.push(tenant.as_str(), &req.table, req.rows).await;
    Json(json!({
        "status": "ok",
        "tenant_id": tenant.as_str(),
        "table": req.table,
        "rows_received": received,
        "buffered_rows": buffered,
    }))
}

async fn ingest_status(State(buffer): State<SharedBuffer>) -> Json<Value> {
    let snap = buffer.status().await;
    Json(json!({
        "status": "ok",
        "buffered_rows": snap.buffered_rows,
        "last_flush": snap.last_flush,
        "total_rows_flushed": snap.total_rows_flushed,
        "total_files_flushed": snap.total_files_flushed,
        "flush_threshold_rows": snap.flush_threshold_rows,
        "flush_interval_seconds": snap.flush_interval_seconds,
    }))
}

#[derive(serde::Deserialize)]
struct DistributedQueryRequest {
    sql: String,
    /// How many partitions to fan the query out across. Defaults to 2.
    #[serde(default)]
    partitions: Option<u32>,
    /// Optional column to hash-partition on; defaults to `Replicate` strategy.
    #[serde(default)]
    hash_column: Option<String>,
}

/// Demonstration endpoint for the scatter-gather executor.
///
/// In production, the workers list would come from the live worker registry
/// (Redis-backed or coordinator scheduler). For demos and CI we fan out the
/// query to N `LocalWorkerExecutor`s sharing the coordinator's engine handle —
/// this exercises the full split → dispatch → merge code path even on a
/// single node.
#[tracing::instrument(
    name = "rest.distributed_query",
    skip(handle, req),
    fields(partitions = req.partitions.unwrap_or(2), sql_len = req.sql.len())
)]
async fn distributed_query(
    State(handle): State<AppState>,
    Json(req): Json<DistributedQueryRequest>,
) -> Json<Value> {
    let n = req.partitions.unwrap_or(2).max(1);
    let strategy = match req.hash_column.as_deref() {
        Some(col) if !col.is_empty() => PartitionStrategy::HashColumn(col.to_string()),
        _ => PartitionStrategy::Replicate,
    };

    let workers: Vec<Arc<dyn WorkerExecutor>> = (0..n)
        .map(|i| {
            Arc::new(LocalWorkerExecutor::with_label(
                handle.clone(),
                format!("local-{i}"),
            )) as Arc<dyn WorkerExecutor>
        })
        .collect();

    let exec = DistributedExecutor::new(workers).with_strategy(strategy);
    match exec.execute(&req.sql).await {
        Ok(batches) => {
            let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
            let rows = batches_to_ndjson(&batches);
            Json(json!({
                "status": "ok",
                "partitions": n,
                "rows": total_rows,
                "data": rows,
            }))
        }
        Err(e) => Json(json!({
            "status": "error",
            "message": e.to_string(),
        })),
    }
}

#[cfg(test)]
mod serialization_tests {
    use super::*;
    use arrow::array::{Int32Array, RecordBatch};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn batch(vals: &[i32]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int32, false)]));
        RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(vals.to_vec()))]).unwrap()
    }

    #[test]
    fn ndjson_serializes_every_batch_not_just_the_first() {
        // Regression: a multi-batch result (e.g. partitioned aggregate output)
        // must serialize all rows, matching the reported total — not only the
        // first batch.
        let batches = vec![batch(&[1, 2, 3]), batch(&[4, 5]), batch(&[6])];
        let total: usize = batches.iter().map(|b| b.num_rows()).sum();
        let out = batches_to_ndjson(&batches);
        let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(
            lines.len(),
            total,
            "expected {total} rows, got {}",
            lines.len()
        );
        assert_eq!(total, 6);
        assert!(out.contains("\"n\":6"), "last batch's row must be present");
    }

    #[test]
    fn ndjson_empty_for_no_batches() {
        assert_eq!(batches_to_ndjson(&[]), "");
    }
}

#[cfg(test)]
mod tenant_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use opensnow_auth::JwtManager;
    use opensnow_core::{EngineConfig, OpenSnowEngine};
    use std::sync::Arc;
    use tower::ServiceExt;

    fn make_engine() -> OpenSnowEngine {
        let dir = tempfile::tempdir().unwrap();
        let warehouse = dir.path().join("wh").to_string_lossy().to_string();
        let catalog_path = dir.path().join("catalog.db").to_string_lossy().to_string();
        let cfg = EngineConfig {
            warehouse_path: warehouse,
            ..Default::default()
        };
        let engine = OpenSnowEngine::from_config_and_catalog(cfg, &catalog_path);
        // Leak the temp dir for the lifetime of the test runtime — the engine
        // owns the catalog and shutting it down is async/best-effort.
        std::mem::forget(dir);
        engine
    }

    fn make_router() -> Router {
        create_router(make_engine())
    }

    fn test_auth_state() -> crate::auth::AuthState {
        let jwt = Arc::new(JwtManager::new(b"test-secret"));
        let clients = crate::auth::ClientRegistry::new();
        clients.register_with_metadata(
            "admin-client",
            "secret-admin",
            "SYSADMIN",
            "default",
            vec!["policy.admin".to_string()],
        );
        clients.register_with_metadata(
            "analyst-client",
            "secret-analyst",
            "ANALYST",
            "default",
            vec!["sql.query".to_string(), "table.select".to_string()],
        );
        crate::auth::AuthState::new(jwt, clients, 1)
    }

    fn bearer(
        state: &crate::auth::AuthState,
        client_id: &str,
        role: &str,
        scopes: Vec<&str>,
    ) -> String {
        let token = state
            .jwt
            .generate_token_with_scopes(
                0,
                client_id,
                role,
                "default",
                scopes.into_iter().map(ToOwned::to_owned).collect(),
                1,
            )
            .unwrap();
        format!("Bearer {token}")
    }

    fn make_auth_router(auth: crate::auth::AuthState) -> Router {
        create_router_with_auth(EngineHandle::spawn(make_engine()), Some(auth))
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn embedded_workspace_ui_does_not_load_third_party_analytics_by_default() {
        assert!(
            !APP_UI.contains("googletagmanager.com")
                && !APP_UI.contains("gtag/js")
                && !APP_UI.contains("G-2B82DV9GZJ"),
            "self-hosted/local workspace UI must not emit GA4 or other third-party analytics by default"
        );
    }

    #[test]
    fn embedded_workspace_ui_explains_unconfigured_pipeline_in_local_default() {
        assert!(
            APP_UI.contains("OPENSNOW_DBT_PROJECT_DIR")
                && APP_UI.contains("No dbt artifacts configured")
                && APP_UI.contains("docs/MCP_CONTROL_PLANE.md"),
            "default local pipeline UI should gate the dbt limitation with operator configuration guidance"
        );
    }

    #[test]
    fn public_test_ui_labels_idempotent_demo_load_as_already_loaded() {
        assert!(
            PUBLIC_TEST_UI.contains("Demo data already loaded")
                && PUBLIC_TEST_UI.contains("Continue to queries"),
            "public-test UI should not report an idempotent load as zero newly loaded rows"
        );
    }

    #[tokio::test]
    async fn serves_open_graph_image_without_auth() {
        let resp = make_router()
            .oneshot(
                Request::builder()
                    .uri("/og-image.png")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("image/png")
        );
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(bytes.len() > 10_000);
    }

    #[tokio::test]
    async fn list_default_tenant_after_startup() {
        let router = make_router();
        let resp = router
            .oneshot(Request::get("/api/v1/tenants").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "ok");
        let tenants = body["tenants"].as_array().unwrap();
        assert!(tenants.iter().any(|t| t["id"] == "default"));
    }

    #[tokio::test]
    async fn create_tenant_then_list() {
        let router = make_router();
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/v1/tenants")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Acme Corp"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let created = body_json(resp).await;
        assert_eq!(created["status"], "ok");
        assert_eq!(created["tenant"]["id"], "acme-corp");

        // The new tenant should appear in the list.
        let resp = router
            .oneshot(Request::get("/api/v1/tenants").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let body = body_json(resp).await;
        let tenants = body["tenants"].as_array().unwrap();
        assert!(tenants.iter().any(|t| t["id"] == "acme-corp"));
    }

    #[tokio::test]
    async fn auth_enabled_tenant_mutation_requires_admin_scope() {
        let auth = test_auth_state();
        let admin = bearer(&auth, "admin-client", "SYSADMIN", vec!["policy.admin"]);
        let app = make_auth_router(auth);

        let unauthenticated = app
            .clone()
            .oneshot(
                Request::post("/api/v1/tenants")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"name": "Acme Corp"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let authenticated_admin = app
            .oneshot(
                Request::post("/api/v1/tenants")
                    .header("content-type", "application/json")
                    .header("authorization", admin)
                    .body(Body::from(r#"{"name": "Acme Corp"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated_admin.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_enabled_sso_login_stays_public() {
        let auth = test_auth_state();
        let app = make_auth_router(auth);

        let resp = app
            .oneshot(
                Request::post("/api/v1/auth/sso/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"email":"user@example.com","redirect_uri":"/api/v1/auth/sso/callback"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["error"], "sso_not_configured_for_domain");
    }

    #[tokio::test]
    async fn query_history_scoped_by_header() {
        let router = make_router();

        // Run two queries — one in tenant `red`, one in tenant `blue`.
        for tenant in ["red", "blue"] {
            let resp = router
                .clone()
                .oneshot(
                    Request::post("/api/v1/query")
                        .header("content-type", "application/json")
                        .header("X-Tenant-ID", tenant)
                        .body(Body::from(format!(
                            r#"{{"sql": "SELECT '{tenant}' AS who"}}"#
                        )))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert_eq!(body["status"], "ok");
        }
    }

    #[tokio::test]
    async fn browser_demo_html_is_public_facing() {
        let router = make_router();
        let resp = router
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(bytes.to_vec()).unwrap();

        // Public-facing console + workspace tabs are present.
        for required in [
            "OpenSnow",
            "SQL console",
            "sample-query",
            "<textarea id=\"sql\"",
            "data-tab=\"build\"",
        ] {
            assert!(
                html.contains(required),
                "missing public console element: {required}"
            );
        }
        // Internal/operator copy must NOT leak onto the public demo.
        for forbidden in [
            "pgwire is disabled by default",
            "--enable-pgwire",
            "OPENSNOW_ENABLE_PGWIRE",
            "psql -h localhost",
            "curl http://localhost:8080/api/v1/query",
            "external tester",
        ] {
            assert!(
                !html.contains(forbidden),
                "internal copy leaked to public demo: {forbidden}"
            );
        }
    }

    #[tokio::test]
    async fn browser_demo_html_exposes_external_tester_onboarding() {
        let router = make_router();
        let resp = router
            .oneshot(Request::get("/public-test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(bytes.to_vec()).unwrap();

        for required in [
            "Run your first OpenSnow query in under a minute.",
            "first-run path for external testers",
            "loads safe demo data",
            "copyable psql/pgwire/API commands",
            "Hosted evaluation sandbox accounts",
        ] {
            assert!(
                html.contains(required),
                "missing onboarding copy for external testers: {required}"
            );
        }
    }

    #[tokio::test]
    async fn browser_demo_docs_links_are_served_from_local_server() {
        let router = make_router();

        for (path, expected) in [
            ("/docs/DEPLOYMENT.md", "# OpenSnow Deployment"),
            (
                "/docs/SQL_COMPATIBILITY.md",
                "# SQL compatibility and known limits",
            ),
            ("/docs/PUBLIC_TEST_PATH.md", "# Public test path"),
        ] {
            let resp = router
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                resp.status(),
                StatusCode::OK,
                "docs path {path} should be reachable"
            );
            let bytes = resp.into_body().collect().await.unwrap().to_bytes();
            let body = String::from_utf8(bytes.to_vec()).unwrap();
            assert!(
                body.contains(expected),
                "docs path {path} should include expected markdown heading {expected}"
            );
        }
    }

    #[tokio::test]
    async fn demo_load_endpoint_creates_queryable_sample_table() {
        let router = make_router();
        let resp = router
            .clone()
            .oneshot(
                Request::post("/api/v1/demo/load")
                    .header("content-type", "application/json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["table"], "opensnow_demo_orders");
        assert!(
            body["sample_query"]
                .as_str()
                .unwrap()
                .contains("opensnow_demo_orders")
        );

        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"SELECT region, SUM(amount) AS revenue FROM opensnow_demo_orders GROUP BY region ORDER BY revenue DESC"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let query_body = body_json(resp).await;
        assert_eq!(query_body["status"], "ok");
        assert!(query_body["rows"].as_i64().unwrap() >= 3);
    }

    #[tokio::test]
    async fn demo_load_endpoint_is_idempotent_when_sample_table_already_exists() {
        let router = make_router();

        for attempt in 1..=2 {
            let resp = router
                .clone()
                .oneshot(
                    Request::post("/api/v1/demo/load")
                        .header("content-type", "application/json")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_json(resp).await;
            assert_eq!(
                body["status"], "ok",
                "demo load attempt {attempt} should succeed even after the table exists: {body}"
            );
            assert_eq!(body["table"], "opensnow_demo_orders");
            if attempt == 1 {
                assert_eq!(body["already_loaded"], false);
                assert_eq!(body["rows_ingested"], 6);
            } else {
                assert_eq!(body["already_loaded"], true);
                assert_eq!(body["rows_ingested"], 0);
                assert_eq!(
                    body["rows_checked"], 6,
                    "rows_checked should report the sample table row count, not the COUNT(*) RecordBatch row count: {body}"
                );
            }
            assert!(
                body["sample_query"]
                    .as_str()
                    .unwrap()
                    .contains("opensnow_demo_orders")
            );
        }

        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"SELECT COUNT(*) AS rows FROM opensnow_demo_orders"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let query_body = body_json(resp).await;
        assert_eq!(query_body["status"], "ok");
        assert!(query_body["data"].as_str().unwrap().contains(r#""rows":6"#));
    }

    #[tokio::test]
    async fn query_accepts_documented_select_with_trailing_semicolon() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"SELECT 1 AS smoke;"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["rows"], 1);
        assert!(body["data"].as_str().unwrap().contains(r#""smoke":1"#));
    }

    #[tokio::test]
    async fn query_rejects_multiple_statements_with_clear_demo_error() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"SELECT 1; SELECT 2"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("OpenSnow demo accepts one SQL statement per request"));
    }

    #[test]
    fn demo_sql_validator_rejects_destructive_and_unsafe_writes() {
        for sql in [
            "DROP TABLE smoke_rollup",
            "DROP MATERIALIZED VIEW mv_demo",
            "COPY INTO public_smoke FROM '/tmp/public_smoke.parquet'",
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
            "CREATE TABLE ../escape AS SELECT 1 AS smoke",
            "CREATE TABLE IF NOT EXISTS ../escape AS SELECT 1 AS smoke",
            "CREATE TABLE public.bad AS SELECT 1 AS smoke",
            "CREATE MATERIALIZED VIEW ../escape AS SELECT 1 AS smoke",
            "CREATE MATERIALIZED VIEW IF NOT EXISTS ../escape AS SELECT 1 AS smoke",
        ] {
            assert!(
                validate_demo_sql(sql).is_err(),
                "demo validator should reject unsafe SQL: {sql}"
            );
        }
    }

    #[test]
    fn demo_sql_validator_accepts_select_materializations_with_whitespace_after_as_keyword() {
        for sql in [
            "CREATE TABLE safe_tab AS\tSELECT 1 AS smoke",
            "CREATE TABLE safe_newline AS\nSELECT 1 AS smoke",
            "CREATE MATERIALIZED VIEW safe_mv_tab AS\tSELECT 1 AS smoke",
            "CREATE MATERIALIZED VIEW safe_mv_newline AS\nSELECT 1 AS smoke",
        ] {
            validate_demo_sql(sql)
                .unwrap_or_else(|err| panic!("demo validator should accept safe SQL {sql}: {err}"));
        }
    }

    #[test]
    fn export_postgres_sql_validator_accepts_only_read_only_result_statements() {
        for sql in [
            "SELECT 1 AS smoke",
            "WITH rows AS (SELECT 1 AS smoke) SELECT * FROM rows",
            "EXPLAIN SELECT 1 AS smoke",
            "SELECT 'DROP TABLE is string literal' AS note",
        ] {
            validate_export_postgres_sql(sql).unwrap_or_else(|err| {
                panic!("export postgres validator should accept read-only SQL {sql}: {err}")
            });
        }

        for sql in [
            "CREATE TABLE safe AS SELECT 1 AS smoke",
            "ALTER WAREHOUSE default SET SIZE='small'",
            "REFRESH MATERIALIZED VIEW mv_demo",
            "USE WAREHOUSE analytics",
            "DROP TABLE smoke_rollup",
            "COPY INTO public_smoke FROM '/tmp/public_smoke.parquet'",
            "SHOW TABLES",
            "DESCRIBE orders_ext",
            "SELECT 1; DROP TABLE smoke_rollup",
        ] {
            assert!(
                validate_export_postgres_sql(sql).is_err(),
                "export postgres validator should reject non-read-only SQL: {sql}"
            );
        }
    }

    #[tokio::test]
    async fn export_postgres_rejects_demo_write_sql_before_target_connection() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/export/postgres")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"CREATE TABLE safe AS SELECT 1 AS smoke","dsn":"postgres://user:***@localhost:1/db","table":"smoke"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("read-only result-producing SQL")
        );
    }

    #[tokio::test]
    async fn query_rejects_drop_table_for_external_demo() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"DROP TABLE smoke_rollup"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("Unsupported SQL statement for the external demo: DROP"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));
    }

    #[tokio::test]
    async fn query_rejects_copy_into_for_external_demo() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"COPY INTO public_smoke FROM '/tmp/public_smoke.parquet'"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("Unsupported SQL statement for the external demo: COPY"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));
    }

    #[tokio::test]
    async fn query_rejects_ctas_with_path_traversal_identifier() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"CREATE TABLE ../escape AS SELECT 1 AS smoke"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("invalid demo table identifier"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));
    }

    #[tokio::test]
    async fn query_rejects_ctas_wrapped_destructive_subquery_before_side_effects() {
        let router = make_router();

        let create_victim = router
            .clone()
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"CREATE TABLE victim AS SELECT 1 AS x"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(body_json(create_victim).await["status"], "ok");

        let wrapped_drop = router
            .clone()
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"CREATE TABLE safe AS DROP TABLE victim"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(wrapped_drop).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("materialization query must start with SELECT or WITH"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));

        let victim_query = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"sql":"SELECT x FROM victim"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(victim_query).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["rows"], 1);
    }

    #[tokio::test]
    async fn query_rejects_ctas_if_not_exists_with_path_traversal_identifier() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"CREATE TABLE IF NOT EXISTS ../escape AS SELECT 1 AS smoke"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("invalid demo table identifier"));
        assert!(message.contains("../escape"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));
    }

    #[test]
    fn table_register_request_validates_identifier_and_uri_surface() {
        for name in ["orders", "orders_2026", "_tmp"] {
            assert!(is_safe_table_name(name), "safe table name rejected: {name}");
        }
        for name in ["", "1orders", "public.orders", "orders;drop", "../orders"] {
            assert!(
                !is_safe_table_name(name),
                "unsafe table name accepted: {name}"
            );
        }

        for uri in [
            "/data/orders.parquet",
            "./demo/orders.parquet",
            "../fixtures/orders.parquet",
            "file:///data/orders.parquet",
            "s3://bucket/orders/",
            "gs://bucket/orders/",
            "az://container/orders/",
        ] {
            assert!(is_supported_parquet_uri(uri), "safe URI rejected: {uri}");
        }
        for uri in ["", "http://example.com/orders.parquet", "s3://bucket/a\nb"] {
            assert!(!is_supported_parquet_uri(uri), "unsafe URI accepted: {uri}");
        }
    }

    #[tokio::test]
    async fn auth_enabled_table_admin_routes_require_policy_admin_scope() {
        let auth = test_auth_state();
        let analyst = bearer(
            &auth,
            "analyst-client",
            "ANALYST",
            vec!["sql.query", "table.select"],
        );
        let app = make_auth_router(auth);

        let unauthenticated = app
            .clone()
            .oneshot(
                Request::post("/api/v1/tables/register")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"orders","uri":"/tmp/orders.parquet"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let non_admin = app
            .oneshot(
                Request::post("/api/v1/export/postgres")
                    .header("content-type", "application/json")
                    .header("authorization", analyst)
                    .body(Body::from(
                        r#"{"sql":"SELECT 1 AS smoke","dsn":"postgres://user:***@localhost/db","table":"smoke"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(non_admin.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn auth_enabled_chart_mutations_require_policy_admin_scope() {
        let auth = test_auth_state();
        let admin = bearer(&auth, "admin-client", "SYSADMIN", vec!["policy.admin"]);
        let analyst = bearer(
            &auth,
            "analyst-client",
            "ANALYST",
            vec!["sql.query", "table.select"],
        );
        let app = make_auth_router(auth);

        let public_read = app
            .clone()
            .oneshot(Request::get("/api/v1/charts").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(public_read.status(), StatusCode::OK);

        let unauthenticated = app
            .clone()
            .oneshot(
                Request::post("/api/v1/charts")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"Unsafe","sql":"SELECT 1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

        let non_admin = app
            .clone()
            .oneshot(
                Request::delete("/api/v1/charts/c1")
                    .header("authorization", analyst)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(non_admin.status(), StatusCode::FORBIDDEN);

        let authenticated_admin = app
            .oneshot(
                Request::post("/api/v1/charts")
                    .header("content-type", "application/json")
                    .header("authorization", admin)
                    .body(Body::from(r#"{"title":"Safe","sql":"SELECT 1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(authenticated_admin.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn query_rejects_unsupported_statement_with_known_limits_hint() {
        let router = make_router();
        let resp = router
            .oneshot(
                Request::post("/api/v1/query")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"sql":"DELETE FROM missing_table WHERE id = 1"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "error");
        let message = body["message"].as_str().unwrap();
        assert!(message.contains("Unsupported SQL statement for the external demo: DELETE"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));
    }
}
