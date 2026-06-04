//! End-to-end integration test for the OpenSnow stack.
#![allow(clippy::await_holding_lock)]
//!
//! Spins up an in-process REST server (axum) and an in-process MCP server
//! on random ports backed by a shared `EngineHandle`. Exercises the golden
//! path: ingest via SQL DDL, query via REST, schema introspection via MCP.
//!
//! Run with:
//!     cargo test -p opensnow-e2e-tests --test integration
//! or:
//!     cargo test --test integration

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use opensnow_auth::{JwtManager, Privilege};
use opensnow_core::{EngineConfig, EngineHandle, OpenSnowEngine};
use opensnow_server::auth::{AuthState, ClientRegistry};
use serde_json::Value;
use tokio::net::TcpListener;

/// Serialise tests that mutate process-global env vars (MCP_JWT_SECRET).
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Spawn the REST router on a random localhost port and return its base URL.
async fn spawn_rest(handle: EngineHandle) -> String {
    let app = opensnow_server::rest::create_router_with_handle(handle);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("rest server died");
    });
    format!("http://{addr}")
}

/// Spawn the MCP router on a random localhost port and return its base URL.
async fn spawn_mcp(handle: EngineHandle) -> String {
    let app = opensnow_mcp::router(handle);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("mcp server died");
    });
    format!("http://{addr}")
}

/// Build an isolated engine with a temp warehouse and catalog.
fn build_engine(warehouse: &str, catalog: &str) -> OpenSnowEngine {
    let config = EngineConfig {
        warehouse_path: warehouse.to_string(),
        ..EngineConfig::default()
    };
    OpenSnowEngine::from_config_and_catalog(config, catalog)
}

#[tokio::test]
async fn rest_health_ingest_query_then_mcp_schema() {
    let _lock = ENV_LOCK.lock().unwrap();
    // Make sure MCP auth is in dev mode (disabled).
    unsafe {
        std::env::remove_var("MCP_AUTH_TOKEN");
        std::env::remove_var("MCP_JWT_SECRET");
    }

    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);

    let rest_url = spawn_rest(handle.clone()).await;
    let mcp_url = spawn_mcp(handle.clone()).await;

    let client = reqwest::Client::new();

    // ── /health ────────────────────────────────────────────────────────────
    let resp = client
        .get(format!("{rest_url}/health"))
        .send()
        .await
        .expect("health request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok", "expected /health to return ok");

    // ── ingest via /api/v1/ingest ──────────────────────────────────────────
    // The ingest endpoint lifts JSON rows into a CTAS that registers the
    // table inside the engine session.
    let resp = client
        .post(format!("{rest_url}/api/v1/ingest"))
        .json(&serde_json::json!({
            "table": "smoke",
            "columns": ["id", "name"],
            "rows": [[1, "alice"], [2, "bob"], [3, "charlie"]],
        }))
        .send()
        .await
        .expect("ingest request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok", "ingest failed: {body}");
    assert_eq!(body["rows_ingested"], 3);

    // ── query ──────────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{rest_url}/api/v1/query"))
        .json(&serde_json::json!({"sql": "SELECT COUNT(*) AS n FROM smoke"}))
        .send()
        .await
        .expect("count query failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["rows"], 1);
    let data = body["data"].as_str().expect("data is string");
    // line-delimited JSON: `{"n":3}\n`
    assert!(data.contains("\"n\":3"), "expected n=3 in data, got {data}");

    // Ordered SELECT to make sure the rows are present.
    let resp = client
        .post(format!("{rest_url}/api/v1/query"))
        .json(&serde_json::json!({"sql": "SELECT id, name FROM smoke ORDER BY id"}))
        .send()
        .await
        .expect("select query failed");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["rows"], 3);
    let data = body["data"].as_str().unwrap();
    assert!(data.contains("alice"));
    assert!(data.contains("bob"));
    assert!(data.contains("charlie"));

    // ── /api/v1/distributed_query ─────────────────────────────────────────
    // Scatter-gather across 3 partitions using the Replicate strategy. The
    // expected result is the same row count as a single SELECT, but the
    // request goes through the partition planner and result-merge codepath.
    let resp = client
        .post(format!("{rest_url}/api/v1/distributed_query"))
        .json(&serde_json::json!({
            "sql": "SELECT id, name FROM smoke",
            "partitions": 3,
        }))
        .send()
        .await
        .expect("distributed_query request failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok", "distributed_query failed: {body}");
    assert_eq!(body["partitions"], 3);
    // Replicate strategy: each of 3 workers runs the same SELECT, so the
    // merged result has 3 × 3 = 9 rows.
    assert_eq!(body["rows"], 9);

    // ── /api/v1/status ─────────────────────────────────────────────────────
    let resp = client
        .get(format!("{rest_url}/api/v1/status"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "running");

    // ── /metrics — Prometheus exposition ──────────────────────────────────
    // Confirm the metrics required by Phase 2 task 3 are all exposed:
    //   - query_count counter (opensnow_queries_total)
    //   - query_duration histogram (opensnow_query_duration_seconds)
    //   - active_warehouses gauge (opensnow_active_warehouses)
    let resp = client
        .get(format!("{rest_url}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let metrics_text = resp.text().await.unwrap();
    for metric in [
        "opensnow_queries_total",
        "opensnow_query_duration_seconds",
        "opensnow_active_warehouses",
    ] {
        assert!(
            metrics_text.contains(metric),
            "metrics missing {metric}:\n{metrics_text}"
        );
    }

    // ── MCP schema listing ─────────────────────────────────────────────────
    // The MCP `/schema/tables` endpoint asks the catalog for tables. The
    // freshly-CTAS'd `smoke` table is registered in the engine session but
    // not in the SQLite catalog, so we instead exercise the MCP `/query`
    // tool which talks to the same `EngineHandle`.
    let resp = client
        .get(format!("{mcp_url}/schema/tables"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["tables"].is_array(),
        "schema/tables should return an array"
    );

    // ── MCP /query ────────────────────────────────────────────────────────
    let resp = client
        .post(format!("{mcp_url}/query"))
        .json(&serde_json::json!({"sql": "SELECT COUNT(*) AS n FROM smoke"}))
        .send()
        .await
        .expect("mcp query failed");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(
        body["status"], "ok",
        "MCP /query returned non-ok status: {body}"
    );
    assert_eq!(body["rows"], 1);
    let data = body["data"].as_str().unwrap();
    assert!(
        data.contains("\"n\":3"),
        "MCP query expected n=3, got {data}"
    );
}

#[tokio::test]
async fn ingest_rejects_invalid_table_name() {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);
    let rest_url = spawn_rest(handle).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{rest_url}/api/v1/ingest"))
        .json(&serde_json::json!({
            "table": "drop; --",
            "columns": ["id"],
            "rows": [[1]],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("invalid table name"),
        "expected invalid-table-name error, got {body}"
    );
}

#[tokio::test]
async fn rest_query_returns_error_for_invalid_sql() {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);
    let rest_url = spawn_rest(handle).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{rest_url}/api/v1/query"))
        .json(&serde_json::json!({"sql": "SELECT * FROM does_not_exist"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "REST returns 200 with error body");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "error");
    assert!(
        body["message"].as_str().unwrap().contains("does_not_exist"),
        "error message should reference missing table: {body}"
    );
}

// ── Auth tests (Task 4) ─────────────────────────────────────────────────────

/// Build a JWT-protected REST router on a random port, returning its base
/// URL plus the `AuthState` so the test can mint tokens directly.
async fn spawn_rest_with_auth(handle: EngineHandle, state: AuthState) -> (String, AuthState) {
    let app = opensnow_server::rest::create_router_with_auth(handle, Some(state.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("rest server died");
    });
    (format!("http://{addr}"), state)
}

fn fresh_handle() -> (EngineHandle, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");
    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    (EngineHandle::spawn(engine), tmp)
}

fn make_auth_state() -> AuthState {
    let jwt = Arc::new(JwtManager::new(b"e2e-jwt-secret-please-do-not-use-in-prod"));
    let clients = ClientRegistry::new();
    clients.register_with_metadata(
        "svc_test",
        "shared_secret",
        "ADMIN",
        "default",
        vec![
            "sql.query".to_string(),
            "table.select".to_string(),
            "ingest.write".to_string(),
            "table.create".to_string(),
            "table.insert".to_string(),
        ],
    );
    let state = AuthState::new(jwt, clients, 1);
    state
        .policy
        .grant_table_privilege("ADMIN", Privilege::Create, "auth_smoke")
        .unwrap();
    state
}

#[tokio::test]
async fn auth_protected_query_rejects_missing_token() {
    let (handle, _tmp) = fresh_handle();
    let (rest_url, _state) = spawn_rest_with_auth(handle, make_auth_state()).await;

    let client = reqwest::Client::new();

    // Public routes still work with no header.
    let h = client
        .get(format!("{rest_url}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(h.status(), 200);
    let m = client
        .get(format!("{rest_url}/metrics"))
        .send()
        .await
        .unwrap();
    assert_eq!(m.status(), 200);

    // Protected route without an Authorization header → 401.
    let q = client
        .post(format!("{rest_url}/api/v1/query"))
        .json(&serde_json::json!({ "sql": "SELECT 1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(q.status(), 401, "missing token should be rejected");
}

#[tokio::test]
async fn auth_protected_query_rejects_invalid_token() {
    let (handle, _tmp) = fresh_handle();
    let (rest_url, _state) = spawn_rest_with_auth(handle, make_auth_state()).await;

    let resp = reqwest::Client::new()
        .post(format!("{rest_url}/api/v1/query"))
        .header("authorization", "Bearer not.a.real.jwt")
        .json(&serde_json::json!({ "sql": "SELECT 1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "garbage token should be rejected");
}

#[tokio::test]
async fn auth_token_endpoint_then_protected_query_succeeds() {
    let (handle, _tmp) = fresh_handle();
    let (rest_url, _state) = spawn_rest_with_auth(handle, make_auth_state()).await;
    let client = reqwest::Client::new();

    // 1. Hit /auth/token with valid client_credentials → get a JWT.
    let token_resp = client
        .post(format!("{rest_url}/auth/token"))
        .json(&serde_json::json!({
            "grant_type": "client_credentials",
            "client_id": "svc_test",
            "client_secret": "shared_secret",
        }))
        .send()
        .await
        .expect("token request failed");
    assert_eq!(token_resp.status(), 200);
    let token_body: Value = token_resp.json().await.unwrap();
    let token = token_body["access_token"].as_str().expect("access_token");
    assert_eq!(token_body["token_type"], "Bearer");
    assert_eq!(
        token_body["scope"],
        "sql.query table.select ingest.write table.create table.insert"
    );

    // 2. Use the token on /api/v1/query → success.
    let resp = client
        .post(format!("{rest_url}/api/v1/query"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({ "sql": "SELECT 7 AS n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["data"].as_str().unwrap().contains("\"n\":7"));

    // 3. Use the token on /api/v1/ingest → success.
    let resp = client
        .post(format!("{rest_url}/api/v1/ingest"))
        .header("authorization", format!("Bearer {token}"))
        .json(&serde_json::json!({
            "table": "auth_smoke",
            "columns": ["id"],
            "rows": [[1], [2]],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert_eq!(body["rows_ingested"], 2);
}

#[tokio::test]
async fn auth_token_endpoint_rejects_bad_client_credentials() {
    let (handle, _tmp) = fresh_handle();
    let (rest_url, _state) = spawn_rest_with_auth(handle, make_auth_state()).await;

    let resp = reqwest::Client::new()
        .post(format!("{rest_url}/auth/token"))
        .json(&serde_json::json!({
            "grant_type": "client_credentials",
            "client_id": "svc_test",
            "client_secret": "WRONG",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn auth_mcp_jwt_secret_gates_query_endpoint() {
    // MCP_JWT_SECRET is process-global, so we serialise tests that touch it.
    let _lock = ENV_LOCK.lock().unwrap();

    let secret = "mcp-e2e-secret";
    // SAFETY: protected by ENV_LOCK above.
    unsafe {
        std::env::set_var("MCP_JWT_SECRET", secret);
        std::env::remove_var("MCP_AUTH_TOKEN");
    }

    let (handle, _tmp) = fresh_handle();
    let mcp_url = spawn_mcp(handle).await;

    let client = reqwest::Client::new();

    // 1. No token → 401.
    let resp = client
        .post(format!("{mcp_url}/query"))
        .json(&serde_json::json!({ "sql": "SELECT 1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // 2. Mint a JWT with the same secret and retry → 200.
    let mgr = JwtManager::new(secret.as_bytes());
    let jwt = mgr
        .generate_token_with_scopes(
            0,
            "svc_test",
            "SYSADMIN",
            "default",
            vec!["sql.query".to_string(), "table.select".to_string()],
            1,
        )
        .expect("generate token");
    let resp = client
        .post(format!("{mcp_url}/query"))
        .header("authorization", format!("Bearer {jwt}"))
        .json(&serde_json::json!({ "sql": "SELECT 1 AS n" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    // SAFETY: protected by ENV_LOCK above.
    unsafe {
        std::env::remove_var("MCP_JWT_SECRET");
    }
}
