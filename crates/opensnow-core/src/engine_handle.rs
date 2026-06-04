/// `EngineHandle` — a `Send + Sync` proxy for `OpenSnowEngine`.
///
/// `OpenSnowEngine` contains a rusqlite `Connection` which is `!Send + !Sync`.
/// We solve this by pinning the engine to a single background Tokio task and
/// communicating via message-passing (oneshot channels).
///
/// The public `EngineHandle` is `Send + Sync` and can be used freely in axum
/// handler state.
use crate::OpenSnowEngine;
use anyhow::Result;
use arrow::array::RecordBatch;
use opensnow_catalog::Tenant;
use tokio::sync::{mpsc, oneshot};

// ── Messages ─────────────────────────────────────────────────────────────────

#[allow(dead_code)]
enum EngineMsg {
    ListTables {
        database: String,
        schema: String,
        reply: oneshot::Sender<Result<Vec<(String, String)>>>,
    },
    ExecuteSql {
        sql: String,
        reply: oneshot::Sender<Result<Vec<RecordBatch>>>,
    },
    RecordQueryHistory {
        tenant_id: String,
        warehouse: String,
        sql: String,
        duration_ms: i64,
        rows: i64,
        status: String,
    },
    ListTenants {
        reply: oneshot::Sender<Result<Vec<Tenant>>>,
    },
    CreateTenant {
        name: String,
        reply: oneshot::Sender<Result<Tenant>>,
    },
    GetTenant {
        id: String,
        reply: oneshot::Sender<Result<Option<Tenant>>>,
    },
    RegisterParquet {
        name: String,
        path: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

// ── Handle ────────────────────────────────────────────────────────────────────

/// `Send + Sync` handle to the engine worker.
#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::Sender<EngineMsg>,
}

impl EngineHandle {
    /// Spawn a background worker task that owns the engine, return a handle.
    pub fn spawn(engine: OpenSnowEngine) -> Self {
        let (tx, mut rx) = mpsc::channel::<EngineMsg>(64);

        // `tokio::task::spawn_local` requires a `LocalSet`. We use
        // `std::thread::spawn` with a single-threaded Tokio runtime instead,
        // so the engine stays on its own OS thread and is never sent anywhere.
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("engine worker runtime");

            rt.block_on(async move {
                if let Err(e) = engine.register_materialized_views().await {
                    tracing::warn!("Failed to register materialized views on startup: {}", e);
                }
                while let Some(msg) = rx.recv().await {
                    match msg {
                        EngineMsg::ListTables {
                            database,
                            schema,
                            reply,
                        } => {
                            let result = engine.catalog().list_tables(&database, &schema);
                            let _ = reply.send(result);
                        }
                        EngineMsg::ExecuteSql { sql, reply } => {
                            let result: Result<_, crate::OpenSnowError> =
                                engine.execute_sql(&sql).await;
                            let result = result.map_err(anyhow::Error::from);
                            let _ = reply.send(result);
                        }
                        EngineMsg::RecordQueryHistory {
                            tenant_id,
                            warehouse,
                            sql,
                            duration_ms,
                            rows,
                            status,
                        } => {
                            engine.record_query_history_for_tenant(
                                &tenant_id,
                                &warehouse,
                                &sql,
                                duration_ms,
                                rows,
                                None,
                                &status,
                            );
                        }
                        EngineMsg::ListTenants { reply } => {
                            let result = engine.catalog().list_tenants();
                            let _ = reply.send(result);
                        }
                        EngineMsg::CreateTenant { name, reply } => {
                            let result = engine.catalog().create_tenant(&name);
                            let _ = reply.send(result);
                        }
                        EngineMsg::GetTenant { id, reply } => {
                            let result = engine.catalog().get_tenant(&id);
                            let _ = reply.send(result);
                        }
                        EngineMsg::RegisterParquet { name, path, reply } => {
                            let result: Result<_, crate::OpenSnowError> =
                                engine.register_parquet(&name, &path).await;
                            let _ = reply.send(result.map_err(anyhow::Error::from));
                        }
                    }
                }
            });
        });

        Self { tx }
    }

    pub async fn list_tables(&self, database: &str, schema: &str) -> Result<Vec<(String, String)>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::ListTables {
                database: database.to_string(),
                schema: schema.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }

    pub async fn execute_sql(&self, sql: &str) -> Result<Vec<RecordBatch>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::ExecuteSql {
                sql: sql.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }

    /// Best-effort query history logging — fire-and-forget. Records under the
    /// default tenant.
    pub async fn record_query_history(
        &self,
        warehouse: &str,
        sql: &str,
        duration_ms: i64,
        rows: i64,
        status: &str,
    ) {
        self.record_query_history_for_tenant(
            opensnow_catalog::DEFAULT_TENANT,
            warehouse,
            sql,
            duration_ms,
            rows,
            status,
        )
        .await;
    }

    /// Best-effort query history logging scoped to a tenant.
    pub async fn record_query_history_for_tenant(
        &self,
        tenant_id: &str,
        warehouse: &str,
        sql: &str,
        duration_ms: i64,
        rows: i64,
        status: &str,
    ) {
        let _ = self
            .tx
            .send(EngineMsg::RecordQueryHistory {
                tenant_id: tenant_id.to_string(),
                warehouse: warehouse.to_string(),
                sql: sql.to_string(),
                duration_ms,
                rows,
                status: status.to_string(),
            })
            .await;
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::ListTenants { reply: reply_tx })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }

    pub async fn create_tenant(&self, name: &str) -> Result<Tenant> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::CreateTenant {
                name: name.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }

    pub async fn get_tenant(&self, id: &str) -> Result<Option<Tenant>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::GetTenant {
                id: id.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }

    /// Register a Parquet file/directory as a named DataFusion table on the
    /// engine's session.
    pub async fn register_parquet(&self, name: &str, path: &str) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(EngineMsg::RegisterParquet {
                name: name.to_string(),
                path: path.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("engine worker died"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("engine reply dropped"))?
    }
}
