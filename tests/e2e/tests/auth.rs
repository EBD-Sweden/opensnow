//! End-to-end JWT auth tests for the REST query API.
//!
//! Spawns the REST router with auth enabled, asks `/auth/token` for a JWT
//! using the OAuth2 client_credentials grant, then exercises both the
//! authenticated and unauthenticated paths against `/api/v1/query`.

use std::net::SocketAddr;
use std::sync::Arc;

use opensnow_auth::{JwtManager, Privilege};
use opensnow_core::{EngineConfig, EngineHandle, OpenSnowEngine};
use opensnow_server::auth::{AuthState, ClientRegistry};
use serde_json::Value;
use tokio::net::TcpListener;

fn build_engine(warehouse: &str, catalog: &str) -> OpenSnowEngine {
    let config = EngineConfig {
        warehouse_path: warehouse.to_string(),
        ..EngineConfig::default()
    };
    OpenSnowEngine::from_config_and_catalog(config, catalog)
}

async fn spawn_rest_with_auth(handle: EngineHandle, auth: AuthState) -> String {
    let app = opensnow_server::rest::create_router_with_auth(handle, Some(auth));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("rest server died");
    });
    format!("http://{addr}")
}

async fn fetch_token(
    client: &reqwest::Client,
    url: &str,
    client_id: &str,
    client_secret: &str,
) -> String {
    let resp = client
        .post(format!("{url}/auth/token"))
        .json(&serde_json::json!({
            "grant_type": "client_credentials",
            "client_id": client_id,
            "client_secret": client_secret,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string()
}

#[tokio::test]
async fn rest_query_requires_jwt_when_auth_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);

    let jwt = Arc::new(JwtManager::new(b"e2e-secret-12345"));
    let clients = ClientRegistry::new();
    clients.register_with_metadata(
        "integration-client",
        "topsecret",
        "ANALYST",
        "default",
        vec![
            "sql.query".to_string(),
            "table.select".to_string(),
            "ingest.write".to_string(),
            "table.create".to_string(),
            "table.insert".to_string(),
        ],
    );
    let auth = AuthState::new(jwt.clone(), clients, 1);
    auth.policy
        .grant_table_privilege("ANALYST", Privilege::Create, "allowed_ingest")
        .unwrap();
    let url = spawn_rest_with_auth(handle, auth).await;

    let client = reqwest::Client::new();

    // ── unauthenticated query is rejected ─────────────────────────────────
    let resp = client
        .post(format!("{url}/api/v1/query"))
        .json(&serde_json::json!({"sql": "SELECT 1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "unauthenticated /query must be 401");

    // ── /health stays public ──────────────────────────────────────────────
    let resp = client.get(format!("{url}/health")).send().await.unwrap();
    assert_eq!(resp.status(), 200);

    // ── client_credentials grant returns a JWT ────────────────────────────
    let resp = client
        .post(format!("{url}/auth/token"))
        .json(&serde_json::json!({
            "grant_type": "client_credentials",
            "client_id": "integration-client",
            "client_secret": "topsecret",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let token = body["access_token"]
        .as_str()
        .expect("access_token")
        .to_string();
    assert_eq!(body["token_type"], "Bearer");
    assert_eq!(
        body["scope"],
        "sql.query table.select ingest.write table.create table.insert"
    );

    // ── authenticated query works ─────────────────────────────────────────
    let resp = client
        .post(format!("{url}/api/v1/query"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"sql": "SELECT 7 AS lucky"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");

    // ── ingest also requires the token ────────────────────────────────────
    let resp = client
        .post(format!("{url}/api/v1/ingest"))
        .json(&serde_json::json!({
            "table": "allowed_ingest",
            "columns": ["id"],
            "rows": [[1]],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    // ── ingest with token succeeds ───────────────────────────────────────
    let resp = client
        .post(format!("{url}/api/v1/ingest"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "table": "allowed_ingest",
            "columns": ["id"],
            "rows": [[1], [2]],
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    assert_eq!(status, 200, "ingest response: {body}");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["rows_ingested"], 2);
}

#[tokio::test]
async fn rest_rejects_invalid_jwt() {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);

    let jwt = Arc::new(JwtManager::new(b"first-secret"));
    let clients = ClientRegistry::new();
    let auth = AuthState::new(jwt, clients, 1);
    let url = spawn_rest_with_auth(handle, auth).await;

    // A token signed with a different secret must be rejected.
    let other = JwtManager::new(b"different-secret");
    let bogus = other.generate_token(1, "alice", "PUBLIC", 1).unwrap();

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{url}/api/v1/query"))
        .bearer_auth(&bogus)
        .json(&serde_json::json!({"sql": "SELECT 1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn dbt_catalog_requires_table_select_scope_when_auth_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let warehouse = tmp.path().join("warehouse");
    std::fs::create_dir_all(&warehouse).unwrap();
    let catalog = tmp.path().join("catalog.db");

    let engine = build_engine(warehouse.to_str().unwrap(), catalog.to_str().unwrap());
    let handle = EngineHandle::spawn(engine);

    let jwt = Arc::new(JwtManager::new(b"e2e-secret-12345"));
    let clients = ClientRegistry::new();
    clients.register_with_metadata(
        "metadata-reader",
        "reader-secret",
        "ANALYST",
        "default",
        vec!["sql.query".to_string(), "table.select".to_string()],
    );
    clients.register_with_metadata(
        "profile-only",
        "profile-secret",
        "ANALYST",
        "default",
        vec!["profile.read".to_string()],
    );
    let auth = AuthState::new(jwt, clients, 1);
    let url = spawn_rest_with_auth(handle, auth).await;
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!("{url}/api/v1/dbt/catalog"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthenticated.status(), 401);

    let weak_token = fetch_token(&client, &url, "profile-only", "profile-secret").await;
    let forbidden = client
        .get(format!("{url}/api/v1/dbt/catalog"))
        .bearer_auth(&weak_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    let reader_token = fetch_token(&client, &url, "metadata-reader", "reader-secret").await;
    let allowed = client
        .get(format!("{url}/api/v1/dbt/catalog"))
        .bearer_auth(&reader_token)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), 200);
}
