//! Pipeline / lineage view.
//!
//! Surfaces the dbt model DAG (`manifest.json`) plus the last run's per-model
//! status (`run_results.json`) so OpenSnow's web UI can visualize each ETL
//! step and its dependencies. Read-only; artifacts are read at request time
//! from `OPENSNOW_DBT_ARTIFACTS_DIR` (default `dbt/target`).

use std::path::PathBuf;

use axum::{Json, Router, response::Html, routing::get};
use serde_json::{Map, Value, json};

const PIPELINE_UI: &str = include_str!("../static/pipeline.html");

pub fn router() -> Router {
    Router::new()
        .route("/pipeline", get(pipeline_ui))
        .route("/api/v1/pipeline", get(pipeline_data))
}

async fn pipeline_ui() -> Html<&'static str> {
    Html(PIPELINE_UI)
}

fn artifacts_dir() -> PathBuf {
    std::env::var("OPENSNOW_DBT_ARTIFACTS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("dbt/target"))
}

fn read_artifact(name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(artifacts_dir().join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Classify a model by naming convention into a pipeline layer.
fn layer_for(name: &str) -> &'static str {
    if name.starts_with("stg_") || name.starts_with("staging_") {
        "staging"
    } else if name.starts_with("mart_") || name.starts_with("fct_") || name.starts_with("dim_") {
        "mart"
    } else {
        "model"
    }
}

/// Build the pipeline graph JSON from dbt artifacts.
async fn pipeline_data() -> Json<Value> {
    let manifest = read_artifact("manifest.json");
    let run_results = read_artifact("run_results.json");

    // unique_id -> (status, execution_time) from the last run.
    let mut status_by_id: Map<String, Value> = Map::new();
    if let Some(results) = run_results
        .as_ref()
        .and_then(|r| r.get("results"))
        .and_then(Value::as_array)
    {
        for r in results {
            if let Some(id) = r.get("unique_id").and_then(Value::as_str) {
                status_by_id.insert(
                    id.to_string(),
                    json!({
                        "status": r.get("status").and_then(Value::as_str).unwrap_or("unknown"),
                        "execution_time": r.get("execution_time").and_then(Value::as_f64),
                    }),
                );
            }
        }
    }
    let generated_at = run_results
        .as_ref()
        .and_then(|r| r.get("metadata"))
        .and_then(|m| m.get("generated_at"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut nodes: Vec<Value> = Vec::new();

    // Sources (the raw inputs).
    if let Some(sources) = manifest
        .as_ref()
        .and_then(|m| m.get("sources"))
        .and_then(Value::as_object)
    {
        for (id, src) in sources {
            nodes.push(json!({
                "id": id,
                "name": src.get("name").and_then(Value::as_str).unwrap_or(id),
                "layer": "source",
                "depends_on": [],
                "status": "source",
                "execution_time": Value::Null,
            }));
        }
    }

    // Models (staging + marts), with dependency edges and run status.
    if let Some(model_nodes) = manifest
        .as_ref()
        .and_then(|m| m.get("nodes"))
        .and_then(Value::as_object)
    {
        for (id, node) in model_nodes {
            if node.get("resource_type").and_then(Value::as_str) != Some("model") {
                continue;
            }
            let name = node.get("name").and_then(Value::as_str).unwrap_or(id);
            let depends_on: Vec<&str> = node
                .get("depends_on")
                .and_then(|d| d.get("nodes"))
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            let (status, exec) = match status_by_id.get(id) {
                Some(s) => (
                    s.get("status").and_then(Value::as_str).unwrap_or("unknown"),
                    s.get("execution_time").cloned().unwrap_or(Value::Null),
                ),
                None => ("not_run", Value::Null),
            };
            nodes.push(json!({
                "id": id,
                "name": name,
                "layer": layer_for(name),
                "depends_on": depends_on,
                "status": status,
                "execution_time": exec,
            }));
        }
    }

    // Optional downstream dashboard link(s), e.g. a Metabase board.
    let dashboards = std::env::var("OPENSNOW_DASHBOARD_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .map(|url| {
            let name = std::env::var("OPENSNOW_DASHBOARD_NAME")
                .unwrap_or_else(|_| "Dashboard".to_string());
            vec![json!({ "name": name, "url": url })]
        })
        .unwrap_or_default();

    Json(json!({
        "available": manifest.is_some(),
        "artifacts_dir": artifacts_dir().to_string_lossy(),
        "generated_at": generated_at,
        "nodes": nodes,
        "dashboards": dashboards,
    }))
}
