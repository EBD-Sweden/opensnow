//! Saved chart / mini-dashboard specs for the native "Build" tab.
//!
//! A tiny file-backed store (a JSON array) so charts created in the UI — or by
//! an LLM via the MCP `chart_create` tool — persist server-side and render in
//! the Dashboards tab. Each spec is declarative (table / columns / aggregate +
//! the generated SQL); rendering is client-side Vega-Lite and execution goes
//! through the gated `/api/v1/query` endpoint, so this store never runs SQL.
//!
//! Path: `OPENSNOW_CHARTS_FILE` (default `charts.json`). Reads are public.
//! Writes remain open only in unauthenticated local/public demos; auth-enabled
//! deployments require a bearer token with `policy.admin` (or platform-admin
//! role) before mutating the shared chart store.

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
        .unwrap_or_else(default_charts_file)
}

#[cfg(not(test))]
fn default_charts_file() -> PathBuf {
    PathBuf::from("charts.json")
}

#[cfg(test)]
static DEFAULT_TEST_CHARTS_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(test)]
fn default_charts_file() -> PathBuf {
    DEFAULT_TEST_CHARTS_FILE
        .get_or_init(|| {
            register_default_test_charts_file_cleanup();
            std::env::temp_dir()
                .join(format!(
                    "opensnow-server-test-charts-{}",
                    std::process::id()
                ))
                .join("charts.json")
        })
        .clone()
}

#[cfg(test)]
fn cleanup_default_test_charts_file() {
    if let Some(path) = DEFAULT_TEST_CHARTS_FILE.get() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        } else {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
fn register_default_test_charts_file_cleanup() {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn atexit(cb: extern "C" fn()) -> i32;
        }

        extern "C" fn cleanup_at_exit() {
            cleanup_default_test_charts_file();
        }

        unsafe {
            let _ = atexit(cleanup_at_exit);
        }
    }
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

pub fn router(auth: Option<crate::auth::AuthState>) -> Router {
    let mut write_routes = Router::new()
        .route("/api/v1/charts", axum::routing::post(save_chart))
        .route("/api/v1/charts/{id}", delete(delete_chart));

    if let Some(auth_state) = auth {
        write_routes = write_routes
            .route_layer(axum::middleware::from_fn(crate::auth::require_admin_scope))
            .route_layer(axum::middleware::from_fn_with_state(
                auth_state,
                crate::auth::jwt_required,
            ));
    }

    Router::new()
        .route("/api/v1/charts", get(list_charts))
        .merge(write_routes)
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
        Ok(_) => (
            StatusCode::OK,
            Json(json!({ "status": "ok", "chart": spec })),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_charts_file_is_outside_repository() {
        unsafe {
            std::env::remove_var("OPENSNOW_CHARTS_FILE");
        }

        assert_ne!(charts_file(), PathBuf::from("charts.json"));
    }

    #[test]
    fn test_default_charts_file_cleanup_removes_test_artifacts() {
        unsafe {
            std::env::remove_var("OPENSNOW_CHARTS_FILE");
        }

        let path = charts_file();
        store(&[json!({ "title": "Safe", "sql": "SELECT 1" })]).unwrap();
        assert!(path.exists());

        cleanup_default_test_charts_file();

        assert!(!path.exists());
        if let Some(parent) = path.parent() {
            assert!(!parent.exists());
        }
    }
}
