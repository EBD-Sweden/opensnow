//! Pipeline / lineage view + a dependency-aware scheduler.
//!
//! Surfaces the dbt model DAG (`manifest.json`) plus the last run's per-model
//! status (`run_results.json`) so OpenSnow's web UI can visualize each ETL
//! step. It can also *run* the pipeline: `dbt run` builds models in dependency
//! order (dbt resolves the DAG from the manifest), on demand or on a schedule.
//!
//! Operator config (trusted/local; default off):
//! - `OPENSNOW_DBT_PROJECT_DIR`   — dbt project dir; enables runs.
//! - `OPENSNOW_DBT_EXECUTABLE`    — dbt binary (default `dbt`).
//! - `OPENSNOW_DBT_SCHEDULE_CRON` — cron expression (5 or 6 field); preferred.
//! - `OPENSNOW_DBT_SCHEDULE_SECS` — fixed interval in seconds (fallback).
//! - `OPENSNOW_DBT_ARTIFACTS_DIR` — manifest/run_results dir (default `<project>/target`).
//! - `OPENSNOW_DASHBOARD_URL` / `OPENSNOW_DASHBOARD_NAME` — downstream dashboard link.
//!
//! The read-only view (`/pipeline`, `GET /api/v1/pipeline`) is public; the
//! trigger (`POST /api/v1/pipeline/run`) is admin-scoped when auth is enabled.

use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::State,
    response::Html,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

#[derive(Clone, Default, serde::Serialize)]
struct RunState {
    running: bool,
    last_started: Option<String>,
    last_finished: Option<String>,
    last_success: Option<bool>,
    last_log_tail: Option<String>,
    next_run: Option<String>,
    /// Human-readable schedule description, e.g. `cron: 0 6 * * *` or `every 600s`.
    schedule: Option<String>,
}

#[derive(Clone)]
struct PipelineState {
    run: Arc<Mutex<RunState>>,
}

/// Read-only public router + admin-scoped trigger router (sharing one state).
pub struct PipelineRouters {
    pub public: Router,
    pub admin: Router,
}

pub fn build() -> PipelineRouters {
    let state = PipelineState {
        run: Arc::new(Mutex::new(RunState::default())),
    };
    if dbt_project_dir().is_some() {
        if let Some((schedule, desc)) = configured_schedule() {
            state.run.lock().expect("run state lock").schedule = Some(desc);
            spawn_scheduler(state.clone(), schedule);
        }
    }
    let public = Router::new()
        .route("/pipeline", get(pipeline_ui))
        .route("/api/v1/pipeline", get(pipeline_data))
        .with_state(state.clone());
    let admin = Router::new()
        .route("/api/v1/pipeline/run", post(run_pipeline))
        .with_state(state);
    PipelineRouters { public, admin }
}

async fn pipeline_ui() -> Html<String> {
    // The pipeline lives inside the unified workspace SPA (opens on the Pipelines tab).
    crate::rest::ui_asset("app.html", crate::rest::APP_UI)
}

// ── config ───────────────────────────────────────────────────────────────────

fn dbt_project_dir() -> Option<PathBuf> {
    std::env::var("OPENSNOW_DBT_PROJECT_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
}

fn dbt_executable() -> String {
    std::env::var("OPENSNOW_DBT_EXECUTABLE").unwrap_or_else(|_| "dbt".to_string())
}

fn artifacts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENSNOW_DBT_ARTIFACTS_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    if let Some(project) = dbt_project_dir() {
        return project.join("target");
    }
    PathBuf::from("dbt/target")
}

/// A resolved schedule: either a fixed interval or a parsed cron expression.
enum Schedule {
    Interval(u64),
    Cron(Box<cron::Schedule>),
}

/// Accept standard 5-field cron (`min hour dom mon dow`) by prepending a
/// seconds field, or pass a 6/7-field expression straight to the `cron` crate.
fn parse_cron(expr: &str) -> Option<cron::Schedule> {
    let normalized = if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    };
    cron::Schedule::from_str(&normalized).ok()
}

fn configured_schedule() -> Option<(Schedule, String)> {
    if let Ok(expr) = std::env::var("OPENSNOW_DBT_SCHEDULE_CRON") {
        let expr = expr.trim().to_string();
        if !expr.is_empty() {
            return match parse_cron(&expr) {
                Some(s) => Some((Schedule::Cron(Box::new(s)), format!("cron: {expr}"))),
                None => {
                    tracing::warn!(
                        "invalid OPENSNOW_DBT_SCHEDULE_CRON '{expr}'; scheduler disabled"
                    );
                    None
                }
            };
        }
    }
    let secs = std::env::var("OPENSNOW_DBT_SCHEDULE_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)?;
    Some((Schedule::Interval(secs), format!("every {secs}s")))
}

fn next_fire(schedule: &Schedule) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Interval(secs) => Some(Utc::now() + chrono::Duration::seconds(*secs as i64)),
        Schedule::Cron(s) => s.upcoming(Utc).next(),
    }
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

// ── runner ───────────────────────────────────────────────────────────────────

/// Trigger a pipeline run (returns immediately; the run proceeds in the
/// background and its status is reflected by `/api/v1/pipeline`).
async fn run_pipeline(State(state): State<PipelineState>) -> Json<Value> {
    let Some(project) = dbt_project_dir() else {
        return Json(json!({
            "status": "not_configured",
            "message": "set OPENSNOW_DBT_PROJECT_DIR to enable pipeline runs",
        }));
    };
    {
        let mut s = state.run.lock().expect("run state lock");
        if s.running {
            return Json(json!({ "status": "already_running" }));
        }
        s.running = true;
        s.last_started = Some(now_iso());
    }
    tokio::spawn(do_dbt_run(state, project));
    Json(json!({ "status": "started" }))
}

/// Run `dbt run` in the project dir and record the outcome. dbt builds models
/// in dependency order from the manifest DAG and writes `run_results.json`,
/// which the pipeline view reads to show per-model status.
async fn do_dbt_run(state: PipelineState, project: PathBuf) {
    let output = tokio::process::Command::new(dbt_executable())
        .args(["run", "--no-partial-parse"])
        .current_dir(&project)
        .env("DBT_PROFILES_DIR", &project)
        .output()
        .await;

    let mut s = state.run.lock().expect("run state lock");
    s.running = false;
    s.last_finished = Some(now_iso());
    match output {
        Ok(out) => {
            s.last_success = Some(out.status.success());
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            s.last_log_tail = Some(tail_lines(&combined, 24));
        }
        Err(e) => {
            s.last_success = Some(false);
            s.last_log_tail = Some(format!("failed to launch dbt: {e}"));
        }
    }
}

fn spawn_scheduler(state: PipelineState, schedule: Schedule) {
    tokio::spawn(async move {
        while let Some(next) = next_fire(&schedule) {
            if let Ok(mut s) = state.run.lock() {
                s.next_run = Some(next.to_rfc3339());
            }
            let wait = (next - Utc::now())
                .to_std()
                .unwrap_or(Duration::from_secs(1));
            tokio::time::sleep(wait).await;

            let Some(project) = dbt_project_dir() else {
                continue;
            };
            {
                let mut s = state.run.lock().expect("run state lock");
                if s.running {
                    continue; // don't overlap with an in-flight run
                }
                s.running = true;
                s.last_started = Some(now_iso());
            }
            do_dbt_run(state.clone(), project.clone()).await;
        }
    });
}

// ── lineage / status ─────────────────────────────────────────────────────────

fn read_artifact(name: &str) -> Option<Value> {
    let text = std::fs::read_to_string(artifacts_dir().join(name)).ok()?;
    serde_json::from_str(&text).ok()
}

fn layer_for(name: &str) -> &'static str {
    if name.starts_with("stg_") || name.starts_with("staging_") {
        "staging"
    } else if name.starts_with("mart_") || name.starts_with("fct_") || name.starts_with("dim_") {
        "mart"
    } else {
        "model"
    }
}

async fn pipeline_data(State(state): State<PipelineState>) -> Json<Value> {
    let manifest = read_artifact("manifest.json");
    let run_results = read_artifact("run_results.json");

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
    if let Some(sources) = manifest
        .as_ref()
        .and_then(|m| m.get("sources"))
        .and_then(Value::as_object)
    {
        for (id, src) in sources {
            nodes.push(json!({
                "id": id,
                "name": src.get("name").and_then(Value::as_str).unwrap_or(id),
                "label": src.get("meta").and_then(|m| m.get("label")).and_then(Value::as_str),
                "description": src.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()),
                "layer": "source",
                "depends_on": [],
                "status": "source",
                "execution_time": Value::Null,
            }));
        }
    }
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
                "label": node.get("meta").and_then(|m| m.get("label")).and_then(Value::as_str),
                "description": node.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()),
                "layer": layer_for(name),
                "depends_on": depends_on,
                "status": status,
                "execution_time": exec,
                // The compiled SQL is exactly what OpenSnow executes (CREATE
                // TABLE AS <sql>); raw_sql keeps the dbt ref()/source() form.
                "sql": node.get("compiled_code").and_then(Value::as_str),
                "raw_sql": node.get("raw_code").and_then(Value::as_str),
            }));
        }
    }

    let dashboards = std::env::var("OPENSNOW_DASHBOARD_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .map(|url| {
            let name = std::env::var("OPENSNOW_DASHBOARD_NAME")
                .unwrap_or_else(|_| "Dashboard".to_string());
            vec![json!({ "name": name, "url": url })]
        })
        .unwrap_or_default();

    let run = state.run.lock().expect("run state lock").clone();

    Json(json!({
        "available": manifest.is_some(),
        "artifacts_dir": artifacts_dir().to_string_lossy(),
        "generated_at": generated_at,
        "runnable": dbt_project_dir().is_some(),
        "run": run,
        "nodes": nodes,
        "dashboards": dashboards,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_five_and_six_field_cron() {
        assert!(parse_cron("0 6 * * *").is_some()); // 5-field, daily 06:00
        assert!(parse_cron("0 0 6 * * *").is_some()); // 6-field
        assert!(parse_cron("not a cron").is_none());
    }

    #[test]
    fn interval_next_fire_is_in_the_future() {
        let n = next_fire(&Schedule::Interval(600)).unwrap();
        assert!(n > Utc::now());
    }
}
