//! Platform control-plane tools.
//!
//! These let an agent (via MCP) or an operator (via the CLI) *manage* the
//! analytics platform, not just query it: read and edit the dbt models that
//! define the pipelines, run the pipeline, and read/update the schedule. They
//! operate on the dbt project pointed to by `OPENSNOW_DBT_PROJECT_DIR`.
//!
//! All edits are plain files in the dbt project, so they compose with the
//! existing pipeline/lineage view and scheduler. Tools that mutate are
//! deliberately small and explicit; running the pipeline is the only action
//! that executes SQL.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use serde_json::{Value, json};

use crate::harness::{AgentContext, Tool};

// ── project helpers ────────────────────────────────────────────────────────

fn project_dir() -> Result<PathBuf> {
    let dir = std::env::var("OPENSNOW_DBT_PROJECT_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow!("OPENSNOW_DBT_PROJECT_DIR is not set; point it at the dbt project")
        })?;
    let p = PathBuf::from(dir);
    if !p.join("dbt_project.yml").exists() {
        return Err(anyhow!("no dbt_project.yml found in {}", p.display()));
    }
    Ok(p)
}

fn models_dir() -> Result<PathBuf> {
    Ok(project_dir()?.join("models"))
}

fn artifacts_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("OPENSNOW_DBT_ARTIFACTS_DIR") {
        if !dir.trim().is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    Ok(project_dir()?.join("target"))
}

/// Model names map 1:1 to `<name>.sql`; keep them filesystem-safe.
fn safe_name(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(name)
    } else {
        Err(anyhow!(
            "invalid model name '{name}': use letters, digits and underscores only"
        ))
    }
}

fn walk_sql(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if dir.is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let p = entry?.path();
            if p.is_dir() {
                walk_sql(&p, out)?;
            } else if p.extension().and_then(|s| s.to_str()) == Some("sql") {
                out.push(p);
            }
        }
    }
    Ok(())
}

fn layer_of(path: &Path) -> &'static str {
    let s = path.to_string_lossy();
    if s.contains("staging") {
        "staging"
    } else if s.contains("mart") {
        "mart"
    } else {
        "model"
    }
}

fn find_model(name: &str) -> Result<Option<PathBuf>> {
    let target = format!("{}.sql", safe_name(name)?);
    let mut files = Vec::new();
    walk_sql(&models_dir()?, &mut files)?;
    Ok(files
        .into_iter()
        .find(|p| p.file_name().and_then(|s| s.to_str()) == Some(target.as_str())))
}

fn str_param<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required string parameter '{key}'"))
}

// ── dbt model tools ────────────────────────────────────────────────────────

/// List every dbt model in the project, with its layer.
pub struct DbtListModelsTool;

#[async_trait::async_trait(?Send)]
impl Tool for DbtListModelsTool {
    fn name(&self) -> &'static str {
        "dbt_list_models"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        let root = models_dir()?;
        let mut files = Vec::new();
        walk_sql(&root, &mut files)?;
        files.sort();
        let models: Vec<Value> = files
            .iter()
            .map(|p| {
                let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                json!({
                    "name": name,
                    "layer": layer_of(p),
                    "path": p.strip_prefix(&root).unwrap_or(p).to_string_lossy(),
                })
            })
            .collect();
        Ok(json!({ "count": models.len(), "models": models }))
    }
}

/// Return the SQL of one model.
pub struct DbtGetModelTool;

#[async_trait::async_trait(?Send)]
impl Tool for DbtGetModelTool {
    fn name(&self) -> &'static str {
        "dbt_get_model"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = str_param(&params, "name")?;
        let path = find_model(name)?.ok_or_else(|| anyhow!("model '{name}' not found"))?;
        let sql = std::fs::read_to_string(&path)?;
        Ok(json!({ "name": name, "layer": layer_of(&path), "sql": sql }))
    }
}

/// Create or overwrite a model's SQL. `layer` chooses the subfolder
/// (staging|marts); defaults to the existing location or `marts` for new models.
pub struct DbtWriteModelTool;

#[async_trait::async_trait(?Send)]
impl Tool for DbtWriteModelTool {
    fn name(&self) -> &'static str {
        "dbt_write_model"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = safe_name(str_param(&params, "name")?)?;
        let sql = str_param(&params, "sql")?;
        let existing = find_model(name)?;
        let path = match (existing, params.get("layer").and_then(Value::as_str)) {
            (Some(p), _) => p, // overwrite in place
            (None, Some(layer)) if layer == "staging" || layer == "marts" => {
                models_dir()?.join(layer).join(format!("{name}.sql"))
            }
            (None, _) => models_dir()?.join("marts").join(format!("{name}.sql")),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, sql)?;
        Ok(json!({
            "status": "written",
            "name": name,
            "layer": layer_of(&path),
            "path": path.strip_prefix(models_dir()?).unwrap_or(&path).to_string_lossy(),
            "note": "Run the pipeline (pipeline_run) to build it.",
        }))
    }
}

/// Delete a model file.
pub struct DbtDeleteModelTool;

#[async_trait::async_trait(?Send)]
impl Tool for DbtDeleteModelTool {
    fn name(&self) -> &'static str {
        "dbt_delete_model"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = str_param(&params, "name")?;
        let path = find_model(name)?.ok_or_else(|| anyhow!("model '{name}' not found"))?;
        std::fs::remove_file(&path)?;
        Ok(json!({ "status": "deleted", "name": name }))
    }
}

// ── pipeline tools ─────────────────────────────────────────────────────────

/// Run the pipeline (`dbt run`) in dependency order. Returns success + log tail.
pub struct PipelineRunTool;

#[async_trait::async_trait(?Send)]
impl Tool for PipelineRunTool {
    fn name(&self) -> &'static str {
        "pipeline_run"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let project = project_dir()?;
        let dbt = std::env::var("OPENSNOW_DBT_EXECUTABLE").unwrap_or_else(|_| "dbt".to_string());
        let mut args = vec!["run".to_string(), "--no-partial-parse".to_string()];
        if let Some(select) = params.get("select").and_then(Value::as_str) {
            args.push("--select".to_string());
            args.push(select.to_string());
        }
        let output = std::process::Command::new(&dbt)
            .args(&args)
            .current_dir(&project)
            .env("DBT_PROFILES_DIR", &project)
            .output()
            .map_err(|e| anyhow!("failed to launch '{dbt}': {e}"))?;
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let tail: Vec<&str> = combined.lines().rev().take(24).collect();
        let tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
        Ok(json!({
            "success": output.status.success(),
            "command": format!("{dbt} {}", args.join(" ")),
            "output_tail": tail,
        }))
    }
}

/// Read the pipeline DAG + last-run status from dbt artifacts.
pub struct PipelineStatusTool;

#[async_trait::async_trait(?Send)]
impl Tool for PipelineStatusTool {
    fn name(&self) -> &'static str {
        "pipeline_status"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        let dir = artifacts_dir()?;
        let read = |f: &str| -> Option<Value> {
            std::fs::read_to_string(dir.join(f))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
        };
        let manifest = read("manifest.json");
        let run_results = read("run_results.json");
        let mut models = Vec::new();
        if let Some(nodes) = manifest
            .as_ref()
            .and_then(|m| m.get("nodes"))
            .and_then(Value::as_object)
        {
            for (id, node) in nodes {
                if node.get("resource_type").and_then(Value::as_str) == Some("model") {
                    models.push(json!({
                        "name": node.get("name").and_then(Value::as_str).unwrap_or(id),
                        "depends_on": node.get("depends_on").and_then(|d| d.get("nodes")).cloned().unwrap_or(json!([])),
                    }));
                }
            }
        }
        let statuses: Vec<Value> = run_results
            .as_ref()
            .and_then(|r| r.get("results"))
            .and_then(Value::as_array)
            .map(|rs| {
                rs.iter()
                    .map(|r| {
                        json!({
                            "node": r.get("unique_id").and_then(Value::as_str),
                            "status": r.get("status").and_then(Value::as_str),
                            "execution_time": r.get("execution_time").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(json!({
            "available": manifest.is_some(),
            "artifacts_dir": dir.to_string_lossy(),
            "model_count": models.len(),
            "models": models,
            "last_run": statuses,
        }))
    }
}

// ── schedule tools ─────────────────────────────────────────────────────────

fn schedule_path() -> Result<PathBuf> {
    Ok(project_dir()?.join("opensnow_schedule.json"))
}

/// Read the configured pipeline schedule (file overlay + env defaults).
pub struct ScheduleGetTool;

#[async_trait::async_trait(?Send)]
impl Tool for ScheduleGetTool {
    fn name(&self) -> &'static str {
        "schedule_get"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        let file = schedule_path()?;
        let from_file: Value = std::fs::read_to_string(&file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(json!({}));
        Ok(json!({
            "file": file.to_string_lossy(),
            "configured": from_file,
            "env_cron": std::env::var("OPENSNOW_DBT_SCHEDULE_CRON").ok(),
            "env_interval_secs": std::env::var("OPENSNOW_DBT_SCHEDULE_SECS").ok(),
        }))
    }
}

/// Set the pipeline schedule: `{ "cron": "0 6 * * *" }` or `{ "interval_secs": 3600 }`.
/// Persists to `opensnow_schedule.json`; the server reads it on next start.
pub struct ScheduleSetTool;

#[async_trait::async_trait(?Send)]
impl Tool for ScheduleSetTool {
    fn name(&self) -> &'static str {
        "schedule_set"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let cron = params.get("cron").and_then(Value::as_str);
        let interval = params.get("interval_secs").and_then(Value::as_u64);
        if cron.is_none() && interval.is_none() {
            return Err(anyhow!(
                "provide either 'cron' (string) or 'interval_secs' (number)"
            ));
        }
        let mut cfg = json!({});
        if let Some(c) = cron {
            cfg["cron"] = json!(c);
        }
        if let Some(i) = interval {
            cfg["interval_secs"] = json!(i);
        }
        let file = schedule_path()?;
        std::fs::write(&file, serde_json::to_string_pretty(&cfg)?)?;
        Ok(json!({
            "status": "saved",
            "file": file.to_string_lossy(),
            "schedule": cfg,
            "note": "Set OPENSNOW_DBT_SCHEDULE_CRON/_SECS from this on server start to activate.",
        }))
    }
}
