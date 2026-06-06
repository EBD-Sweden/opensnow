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

// ── dashboard tools (Metabase) ─────────────────────────────────────────────

fn mb_base() -> String {
    std::env::var("METABASE_URL")
        .unwrap_or_else(|_| "https://metabase.ebdsweden.com".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn mb_creds() -> Result<(String, String)> {
    let user = std::env::var("MB_USER")
        .or_else(|_| std::env::var("METABASE_USER"))
        .map_err(|_| anyhow!("set MB_USER (Metabase admin email)"))?;
    let pw = std::env::var("MB_PASSWORD")
        .or_else(|_| std::env::var("METABASE_PASSWORD"))
        .map_err(|_| anyhow!("set MB_PASSWORD (Metabase admin password)"))?;
    Ok((user, pw))
}

/// One Metabase REST call. Returns parsed JSON, or an error carrying the body.
fn mb_call(method: &str, url: &str, session: Option<&str>, body: Option<Value>) -> Result<Value> {
    let mut req = ureq::request(method, url).set("Content-Type", "application/json");
    if let Some(s) = session {
        req = req.set("X-Metabase-Session", s);
    }
    let resp = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match resp {
        Ok(r) => Ok(r.into_json::<Value>().unwrap_or_else(|_| json!({}))),
        Err(ureq::Error::Status(code, r)) => {
            let txt = r.into_string().unwrap_or_default();
            Err(anyhow!(
                "metabase {method} -> {code}: {}",
                &txt[..txt.len().min(300)]
            ))
        }
        Err(e) => Err(anyhow!("metabase request failed: {e}")),
    }
}

fn mb_login() -> Result<(String, String)> {
    let base = mb_base();
    let (u, p) = mb_creds()?;
    let r = mb_call(
        "POST",
        &format!("{base}/api/session"),
        None,
        Some(json!({ "username": u, "password": p })),
    )?;
    let sid = r
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("metabase login failed (check MB_USER/MB_PASSWORD)"))?
        .to_string();
    Ok((base, sid))
}

/// List existing Metabase dashboards (with public URLs where shared).
pub struct DashboardListTool;

#[async_trait::async_trait(?Send)]
impl Tool for DashboardListTool {
    fn name(&self) -> &'static str {
        "dashboard_list"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        let (base, sid) = mb_login()?;
        let list = mb_call("GET", &format!("{base}/api/dashboard"), Some(&sid), None)?;
        let arr = list.as_array().cloned().unwrap_or_default();
        let out: Vec<Value> = arr
            .iter()
            .filter(|d| d.get("archived").and_then(Value::as_bool) != Some(true))
            .map(|d| {
                json!({
                    "id": d.get("id"),
                    "name": d.get("name"),
                    "public_url": d.get("public_uuid").and_then(Value::as_str)
                        .map(|u| format!("{base}/public/dashboard/{u}")),
                })
            })
            .collect();
        Ok(json!({ "count": out.len(), "dashboards": out }))
    }
}

/// Create a published Metabase dashboard from native-SQL card specs.
///
/// Params: `{ name, description?, cards: [{ title, sql, display?, dimensions?,
/// metrics?, stacked? }] }`. SQL runs against the Postgres serving DB. Returns
/// the public dashboard URL.
pub struct DashboardCreateTool;

#[async_trait::async_trait(?Send)]
impl Tool for DashboardCreateTool {
    fn name(&self) -> &'static str {
        "dashboard_create"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let name = str_param(&params, "name")?;
        let cards = params
            .get("cards")
            .and_then(Value::as_array)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| anyhow!("'cards' must be a non-empty array of {{title, sql}}"))?;
        let (base, sid) = mb_login()?;

        // Public sharing must be on for the public link to resolve.
        let _ = mb_call(
            "PUT",
            &format!("{base}/api/setting/enable-public-sharing"),
            Some(&sid),
            Some(json!({ "value": true })),
        );

        let dbs = mb_call("GET", &format!("{base}/api/database"), Some(&sid), None)?;
        let db_list = dbs
            .get("data")
            .and_then(Value::as_array)
            .cloned()
            .or_else(|| dbs.as_array().cloned())
            .unwrap_or_default();
        let db_id = db_list
            .iter()
            .find(|d| d.get("engine").and_then(Value::as_str) == Some("postgres"))
            .and_then(|d| d.get("id"))
            .cloned()
            .ok_or_else(|| anyhow!("no Postgres database connected in Metabase"))?;

        let mut card_ids = Vec::new();
        for c in cards {
            let title = c
                .get("title")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("each card needs a 'title'"))?;
            let sql = c
                .get("sql")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("each card needs a 'sql'"))?;
            let display = c.get("display").and_then(Value::as_str).unwrap_or("table");
            let mut vs = json!({});
            if let Some(d) = c.get("dimensions") {
                vs["graph.dimensions"] = d.clone();
            }
            if let Some(m) = c.get("metrics") {
                vs["graph.metrics"] = m.clone();
            }
            if c.get("stacked").and_then(Value::as_bool) == Some(true) {
                vs["stackable.stack_type"] = json!("stacked");
            }
            let created = mb_call(
                "POST",
                &format!("{base}/api/card"),
                Some(&sid),
                Some(json!({
                    "name": title,
                    "dataset_query": {"type": "native", "native": {"query": sql}, "database": db_id},
                    "display": display,
                    "visualization_settings": vs,
                })),
            )?;
            card_ids.push(created.get("id").cloned().unwrap_or(Value::Null));
        }

        let dash = mb_call(
            "POST",
            &format!("{base}/api/dashboard"),
            Some(&sid),
            Some(json!({
                "name": name,
                "description": params.get("description").and_then(Value::as_str)
                    .unwrap_or("Created via the OpenSnow control plane."),
            })),
        )?;
        let did = dash
            .get("id")
            .cloned()
            .ok_or_else(|| anyhow!("dashboard creation failed"))?;

        let positions = [(0, 0), (12, 0), (0, 8), (12, 8), (0, 16), (12, 16)];
        let dashcards: Vec<Value> = card_ids
            .iter()
            .enumerate()
            .map(|(i, cid)| {
                let (col, row) = positions[i % positions.len()];
                json!({ "id": -(i as i64 + 1), "card_id": cid, "col": col, "row": row, "size_x": 12, "size_y": 8 })
            })
            .collect();
        mb_call(
            "PUT",
            &format!("{base}/api/dashboard/{did}/cards"),
            Some(&sid),
            Some(json!({ "cards": dashcards })),
        )?;

        let pub_link = mb_call(
            "POST",
            &format!("{base}/api/dashboard/{did}/public_link"),
            Some(&sid),
            Some(json!({})),
        )?;
        let uuid = pub_link
            .get("uuid")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("could not enable public link"))?;

        Ok(json!({
            "status": "created",
            "name": name,
            "dashboard_id": did,
            "cards": card_ids.len(),
            "public_url": format!("{base}/public/dashboard/{uuid}"),
        }))
    }
}

// ── native chart tools (OpenSnow Build tab) ────────────────────────────────

fn opensnow_base() -> String {
    std::env::var("OPENSNOW_HTTP")
        .unwrap_or_else(|_| "http://localhost:8080".to_string())
        .trim_end_matches('/')
        .to_string()
}

fn os_call(method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let url = format!("{}{}", opensnow_base(), path);
    let req = ureq::request(method, &url).set("content-type", "application/json");
    let resp = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match resp {
        Ok(r) => Ok(r.into_json::<Value>().unwrap_or_else(|_| json!({}))),
        Err(ureq::Error::Status(code, r)) => {
            let t = r.into_string().unwrap_or_default();
            Err(anyhow!(
                "opensnow {method} {path} -> {code}: {}",
                &t[..t.len().min(200)]
            ))
        }
        Err(e) => Err(anyhow!("opensnow request failed: {e}")),
    }
}

fn validate_sql_identifier<'a>(kind: &str, value: &'a str) -> Result<&'a str> {
    let ok = !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .enumerate()
            .all(|(idx, c)| c == '_' || c.is_ascii_alphabetic() || (idx > 0 && c.is_ascii_digit()));
    if ok {
        Ok(value)
    } else {
        Err(anyhow!(
            "invalid chart {kind} '{value}': use unquoted SQL identifiers (letters, digits and underscores; not starting with a digit)"
        ))
    }
}

fn validate_chart_type(ctype: &str) -> Result<&str> {
    match ctype {
        "bar" | "line" | "area" | "point" | "arc" | "table" => Ok(ctype),
        _ => Err(anyhow!(
            "invalid chart type '{ctype}': expected bar, line, area, point, arc or table"
        )),
    }
}

fn validate_chart_agg(agg: &str) -> Result<&str> {
    match agg {
        "sum" | "avg" | "max" | "min" | "count" | "none" => Ok(agg),
        _ => Err(anyhow!(
            "invalid chart aggregate '{agg}': expected sum, avg, max, min, count or none"
        )),
    }
}

/// Build the same SELECT the Build tab generates, from declarative fields.
fn build_chart_sql(
    table: &str,
    ctype: &str,
    x: &str,
    y: &str,
    agg: &str,
    series: &str,
    limit: u64,
) -> Result<String> {
    let table = validate_sql_identifier("table", table)?;
    let ctype = validate_chart_type(ctype)?;
    let agg = validate_chart_agg(agg)?;
    let limit = limit.clamp(1, 10_000);

    if ctype == "table" {
        return Ok(format!("SELECT * FROM {table} LIMIT {limit}"));
    }

    let x = validate_sql_identifier("x column", x)?;
    let y = validate_sql_identifier("y column", y)?;
    let series = if series.is_empty() {
        None
    } else {
        Some(validate_sql_identifier("series column", series)?)
    };

    let ycol = match agg {
        "count" => "count(*)".to_string(),
        "none" => y.to_string(),
        a => format!("{a}({y})"),
    };
    let mut cols = format!("{x} AS x, {ycol} AS y");
    if let Some(series) = series {
        cols.push_str(&format!(", {series} AS series"));
    }
    let mut sql = format!("SELECT {cols} FROM {table} WHERE {x} IS NOT NULL");
    if agg != "none" && agg != "count" {
        sql.push_str(&format!(" AND {y} IS NOT NULL"));
    }
    if agg != "none" {
        sql.push_str(&format!(" GROUP BY {x}"));
        if let Some(series) = series {
            sql.push_str(&format!(", {series}"));
        }
    }
    sql.push_str(if matches!(ctype, "line" | "area" | "point") {
        " ORDER BY 1"
    } else {
        " ORDER BY 2 DESC"
    });
    Ok(format!("{sql} LIMIT {limit}"))
}

/// List saved native-Build charts.
pub struct ChartListTool;

#[async_trait::async_trait(?Send)]
impl Tool for ChartListTool {
    fn name(&self) -> &'static str {
        "chart_list"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, _params: Value) -> Result<Value> {
        os_call("GET", "/api/v1/charts", None)
    }
}

/// Create a saved chart on OpenSnow's native Build board.
///
/// Params: `{ title, table, type(bar|line|area|point|arc|table), x, y,
/// agg(sum|avg|max|min|count|none), series?, limit?, sql? }`. If `sql` is
/// omitted it is generated from the fields. Renders in the Dashboards tab.
pub struct ChartCreateTool;

#[async_trait::async_trait(?Send)]
impl Tool for ChartCreateTool {
    fn name(&self) -> &'static str {
        "chart_create"
    }
    async fn invoke(&self, _ctx: &mut AgentContext, params: Value) -> Result<Value> {
        let title = str_param(&params, "title")?;
        let ctype = params.get("type").and_then(Value::as_str).unwrap_or("bar");
        let x = params.get("x").and_then(Value::as_str).unwrap_or("");
        let y = params.get("y").and_then(Value::as_str).unwrap_or("");
        let agg = params.get("agg").and_then(Value::as_str).unwrap_or("sum");
        let series = params.get("series").and_then(Value::as_str).unwrap_or("");
        let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(500);
        let table = params.get("table").and_then(Value::as_str).unwrap_or("");

        let sql = match params.get("sql").and_then(Value::as_str) {
            Some(s) if !s.trim().is_empty() => s.to_string(),
            _ => {
                if table.is_empty() || (ctype != "table" && (x.is_empty() || y.is_empty())) {
                    return Err(anyhow!(
                        "provide 'sql', or 'table' + 'x' + 'y' to generate it"
                    ));
                }
                build_chart_sql(table, ctype, x, y, agg, series, limit)?
            }
        };
        let spec = json!({
            "title": title, "type": ctype, "table": table,
            "x": x, "y": y, "series": series,
            "hasSeries": !series.is_empty(), "sql": sql,
        });
        os_call("POST", "/api/v1/charts", Some(spec))
    }
}

#[cfg(test)]
mod chart_sql_tests {
    use super::*;

    #[test]
    fn generated_chart_sql_accepts_known_types_and_aggregates() {
        let sql = build_chart_sql(
            "mart_house_price_yoy",
            "line",
            "period",
            "yoy_pct",
            "avg",
            "geo",
            250,
        )
        .unwrap();

        assert_eq!(
            sql,
            "SELECT period AS x, avg(yoy_pct) AS y, geo AS series FROM mart_house_price_yoy WHERE period IS NOT NULL AND yoy_pct IS NOT NULL GROUP BY period, geo ORDER BY 1 LIMIT 250"
        );
    }

    #[test]
    fn generated_chart_sql_rejects_unsafe_identifiers_and_enums() {
        for (table, ctype, x, y, agg, series) in [
            ("orders;DROP", "bar", "period", "amount", "sum", ""),
            ("orders", "bar;DROP", "period", "amount", "sum", ""),
            (
                "orders",
                "bar",
                "period) FROM users --",
                "amount",
                "sum",
                "",
            ),
            ("orders", "bar", "period", "amount", "sum);DROP", ""),
            ("orders", "bar", "period", "amount", "sum", "geo;DROP"),
        ] {
            assert!(
                build_chart_sql(table, ctype, x, y, agg, series, 500).is_err(),
                "unsafe chart SQL input should be rejected: {table}/{ctype}/{x}/{y}/{agg}/{series}"
            );
        }
    }

    #[test]
    fn generated_table_sql_clamps_limit_and_validates_table() {
        assert_eq!(
            build_chart_sql("orders", "table", "", "", "sum", "", 50_000).unwrap(),
            "SELECT * FROM orders LIMIT 10000"
        );
        assert!(build_chart_sql("1orders", "table", "", "", "sum", "", 10).is_err());
    }
}
