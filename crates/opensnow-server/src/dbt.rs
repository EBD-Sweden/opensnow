//! dbt catalog endpoint.
//!
//! Returns JSON in the same shape that `dbt docs generate` produces, so
//! external tools (dashboards, lineage UIs, LLM agents) can auto-discover
//! the OpenSnow schema without touching the SQLite catalog directly.
//!
//! Schema reference:
//!   https://schemas.getdbt.com/dbt/catalog/v1/manifest.json

use arrow::array::{Array, StringArray};
use arrow::record_batch::RecordBatch;
use axum::{Extension, Json, Router, extract::State, routing::get};
use opensnow_core::EngineHandle;
use serde_json::{Map, Value, json};

use crate::{
    auth::AuthContext,
    policy::{ObjectPolicyStore, PolicyDecision},
    rest::AppState,
};

const DBT_CATALOG_SCHEMA: &str = "https://schemas.getdbt.com/dbt/catalog/v1/manifest.json";

pub fn router(handle: EngineHandle) -> Router {
    Router::new()
        .route("/api/v1/dbt/catalog", get(catalog))
        .with_state(handle)
}

async fn catalog(
    State(handle): State<AppState>,
    auth: Option<Extension<AuthContext>>,
    policy: Option<Extension<ObjectPolicyStore>>,
) -> Json<Value> {
    let nodes = match build_nodes_for_auth(
        &handle,
        auth.as_ref().map(|ext| &ext.0),
        policy.as_ref().map(|ext| &ext.0),
    )
    .await
    {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("failed to build dbt catalog: {}", e);
            Map::new()
        }
    };

    Json(json!({
        "metadata": {
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "dbt_schema_version": DBT_CATALOG_SCHEMA,
        },
        "nodes": nodes,
        "sources": {},
    }))
}

async fn build_nodes_for_auth(
    handle: &EngineHandle,
    auth: Option<&AuthContext>,
    policy: Option<&ObjectPolicyStore>,
) -> anyhow::Result<Map<String, Value>> {
    // Pull every (table, column, type, ordinal) row in one shot. We sort by
    // ordinal_position so the per-table column index is the array index.
    let sql = "SELECT table_name, column_name, data_type, ordinal_position \
               FROM information_schema.columns \
               WHERE table_schema = 'public' \
               ORDER BY table_name, ordinal_position";

    let batches = handle.execute_sql(sql).await?;

    let mut nodes: Map<String, Value> = Map::new();
    for batch in &batches {
        let rows = batch.num_rows();
        if rows == 0 {
            continue;
        }
        let table_names = string_column(batch, "table_name")?;
        let column_names = string_column(batch, "column_name")?;
        let data_types = string_column(batch, "data_type")?;

        for i in 0..rows {
            let table = table_names.value(i).to_string();
            if !can_read_table(auth, policy, &table) {
                continue;
            }
            let column = column_names.value(i).to_string();
            let dtype = data_types.value(i).to_string();

            let entry = nodes.entry(table.clone()).or_insert_with(|| {
                json!({
                    "metadata": {
                        "type": "table",
                        "name": table,
                    },
                    "columns": {},
                })
            });

            let cols_obj = entry
                .get_mut("columns")
                .and_then(|v| v.as_object_mut())
                .expect("columns is always an object");

            let index = cols_obj.len() as i64;
            cols_obj.insert(
                column,
                json!({
                    "type": dtype,
                    "index": index,
                }),
            );
        }
    }

    Ok(nodes)
}

fn can_read_table(
    auth: Option<&AuthContext>,
    policy: Option<&ObjectPolicyStore>,
    table: &str,
) -> bool {
    let (Some(auth), Some(policy)) = (auth, policy) else {
        return true;
    };
    if !is_safe_table_identifier(table) {
        return false;
    }
    matches!(
        policy.check_sql(auth, &format!("SELECT * FROM {table}")),
        PolicyDecision::Allow
    ) || auth.has_scope("policy.admin")
        || auth.has_scope("*")
}

fn is_safe_table_identifier(table: &str) -> bool {
    !table.is_empty()
        && table
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && table
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a StringArray> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|_| anyhow::anyhow!("missing column {name} in information_schema result"))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("column {name} is not Utf8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensnow_core::OpenSnowEngine;

    #[tokio::test]
    async fn empty_catalog_has_metadata() {
        let engine = OpenSnowEngine::new();
        let handle = EngineHandle::spawn(engine);

        let nodes = build_nodes_for_auth(&handle, None, None).await.unwrap();
        // information_schema columns themselves shouldn't appear (we filter
        // to schema = 'public').
        assert!(!nodes.contains_key("columns"));
    }
}
