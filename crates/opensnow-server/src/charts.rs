//! Saved chart / mini-dashboard specs for the native "Build" tab.
//!
//! A tiny file-backed store (a JSON array) so charts created in the UI — or by
//! an LLM via the MCP `chart_create` tool — persist server-side and render in
//! the Dashboards tab. Each spec is declarative (table / columns / aggregate +
//! the generated SQL); rendering is client-side Vega-Lite and execution goes
//! through the gated `/api/v1/query` endpoint, so this store never runs SQL.
//!
//! Path: `OPENSNOW_CHARTS_FILE` (default `charts.json`). Reads are public; in
//! the public demo, writes are reachable (low-risk: a stored spec is inert).

use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::Path,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get},
};
use serde_json::{Value, json};

const MAX_CHARTS: usize = 500;

fn charts_file() -> PathBuf {
    std::env::var("OPENSNOW_CHARTS_FILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("charts.json"))
}

fn load() -> Vec<Value> {
    std::fs::read_to_string(charts_file())
        .ok()
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
}

fn store(charts: &[Value]) -> std::io::Result<()> {
    let path = charts_file();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(path, serde_json::to_string_pretty(charts)?)
}

pub fn router() -> Router {
    Router::new()
        .route("/api/v1/charts", get(list_charts).post(save_chart))
        .route("/api/v1/charts/{id}", delete(delete_chart))
}

async fn list_charts() -> Json<Value> {
    Json(json!({ "charts": load() }))
}

/// Upsert a chart spec. Requires `title` and `sql`; assigns an `id` if absent.
async fn save_chart(Json(mut spec): Json<Value>) -> impl IntoResponse {
    if spec.get("title").and_then(Value::as_str).is_none()
        || spec.get("sql").and_then(Value::as_str).is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "error", "message": "title and sql are required" })),
        );
    }
    let mut charts = load();
    let id = spec
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("c{}", chrono::Utc::now().timestamp_millis()));
    spec["id"] = json!(id);
    if spec.get("created_at").is_none() {
        spec["created_at"] = json!(chrono::Utc::now().to_rfc3339());
    }
    if let Some(pos) = charts
        .iter()
        .position(|c| c.get("id").and_then(Value::as_str) == Some(id.as_str()))
    {
        charts[pos] = spec.clone();
    } else if charts.len() >= MAX_CHARTS {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "error", "message": "chart limit reached" })),
        );
    } else {
        charts.push(spec.clone());
    }
    match store(&charts) {
        Ok(_) => (StatusCode::OK, Json(json!({ "status": "ok", "chart": spec }))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "message": e.to_string() })),
        ),
    }
}

async fn delete_chart(Path(id): Path<String>) -> Json<Value> {
    let mut charts = load();
    let before = charts.len();
    charts.retain(|c| c.get("id").and_then(Value::as_str) != Some(id.as_str()));
    let removed = before - charts.len();
    let _ = store(&charts);
    Json(json!({ "status": "ok", "removed": removed }))
}
