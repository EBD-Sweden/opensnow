use std::fmt::Debug;
use std::sync::Arc;
use std::time::Instant;

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, SchemaRef};
use async_trait::async_trait;
use futures::sink::Sink;
use futures::stream;
use futures::{SinkExt, StreamExt};
use opensnow_auth::{AuditEvent, AuditResult};
use opensnow_catalog::Catalog;
use opensnow_core::EngineHandle;
use pgwire::api::auth::{self, DefaultServerParameterProvider, StartupHandler};
use pgwire::api::copy::CopyHandler;
use pgwire::api::portal::Portal;
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{
    DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat, FieldInfo,
    QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{NoopQueryParser, StoredStatement};
use pgwire::api::{
    ClientInfo, METADATA_DATABASE, METADATA_USER, NoopErrorHandler, PgWireConnectionState,
    PgWireServerHandlers, Type,
};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::copy::{CopyData, CopyDone, CopyFail};
use pgwire::messages::response::ErrorResponse;
use pgwire::messages::{PgWireBackendMessage, PgWireFrontendMessage};
use serde_json::{Map, Value, json};
use tracing::warn;

use crate::auth::{AuthContext, AuthState};
use crate::metrics::{
    dec_active_queries, dec_warehouse_pending, inc_active_queries, inc_warehouse_pending,
    record_query,
};
use crate::policy::{ObjectPolicyStore, PolicyDecision};
use crate::sql_guardrails::validate_demo_sql;

const PGWIRE_AUTH_CONTEXT_METADATA: &str = "opensnow.auth_context";

fn pg_user_error(code: &str, message: impl Into<String>) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_string(),
        code.to_string(),
        message.into(),
    )))
}

pub struct OpenSnowPgHandler {
    handle: EngineHandle,
    auth: Option<AuthState>,
}

impl OpenSnowPgHandler {
    pub fn new(handle: EngineHandle, auth: Option<AuthState>) -> Self {
        Self { handle, auth }
    }
}

fn serialize_auth_context(auth: &AuthContext) -> String {
    serde_json::to_string(&json!({
        "user_id": auth.user_id,
        "username": auth.username,
        "role": auth.role,
        "tenant_id": auth.tenant_id,
        "scopes": auth.scopes,
    }))
    .expect("AuthContext JSON serializes")
}

fn deserialize_auth_context(raw: &str) -> Result<AuthContext, String> {
    let value: Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
    let scopes = value
        .get("scopes")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(AuthContext {
        user_id: value
            .get("user_id")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        username: value
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        role: value
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        tenant_id: value
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_string(),
        scopes,
    })
}

fn authenticate_pgwire_bearer(
    state: &AuthState,
    startup_user: &str,
    startup_database: Option<&str>,
    token: &str,
) -> Result<AuthContext, String> {
    let claims = state.jwt.validate_token(token).map_err(|e| e.to_string())?;
    if claims.username != startup_user {
        return Err("pgwire startup user does not match bearer subject".to_string());
    }
    if let Some(database) = startup_database.map(str::trim).filter(|s| !s.is_empty()) {
        if database != claims.tenant_id {
            return Err("pgwire startup database/account does not match bearer tenant".to_string());
        }
    }

    if claims.auth_method.as_deref() == Some("oidc") {
        let session_id = claims
            .session_id
            .as_deref()
            .ok_or_else(|| "OIDC pgwire token missing durable session id".to_string())?;
        let sso_path = state
            .sso_session_store_path
            .as_deref()
            .ok_or_else(|| "OIDC pgwire token requires OPENSNOW_SSO_DB_PATH".to_string())?;
        let sso = opensnow_auth::SsoManager::open(sso_path).map_err(|e| e.to_string())?;
        sso.validate_sso_session(session_id, &claims.tenant_id, &claims.username)
            .map_err(|e| e.to_string())?;
    } else {
        if let Some(catalog_path) = state.durable_service_catalog_path.as_deref() {
            state
                .clients
                .refresh_durable_client_from_catalog(catalog_path, &claims.username)?;
        }
        state
            .clients
            .authorize_bearer_client(&claims.username, &claims.scopes)
            .map_err(|status| format!("bearer client is not authorized: {status}"))?;
    }
    Ok(AuthContext::from(claims))
}

fn authorize_pgwire_sql(
    policy: &ObjectPolicyStore,
    auth: &AuthContext,
    sql: &str,
) -> Result<(), String> {
    if !auth.has_all_scopes(&["sql.query", "table.select"]) {
        return Err("pgwire query requires sql.query and table.select scopes".to_string());
    }
    match policy.check_sql(auth, sql) {
        PolicyDecision::Allow => Ok(()),
        PolicyDecision::Deny(denial) => Err(denial.message()),
    }
}

fn append_pgwire_audit(
    state: &AuthState,
    auth: &AuthContext,
    sql: &str,
    result: AuditResult,
    reason: Option<&str>,
) {
    let Some(catalog_path) = state.durable_service_catalog_path.as_deref() else {
        return;
    };
    let Ok(catalog) = Catalog::open(catalog_path) else {
        warn!("failed to open catalog for pgwire audit emission");
        return;
    };
    let mut metadata = Map::new();
    metadata.insert("surface".to_string(), Value::String("pgwire".to_string()));
    metadata.insert("sql_len".to_string(), Value::from(sql.len() as u64));
    if let Some(reason) = reason {
        metadata.insert("reason".to_string(), Value::String(reason.to_string()));
    }
    let event = AuditEvent {
        event_time: chrono::Utc::now(),
        organization_id: auth.tenant_id.clone(),
        tenant_id: Some(auth.tenant_id.clone()),
        actor_type: "user".to_string(),
        actor_id: auth.username.clone(),
        actor_display: Some(auth.username.clone()),
        actor_auth_method: Some("pgwire_jwt".to_string()),
        action: "sql.query".to_string(),
        resource_type: "query".to_string(),
        resource_id: format!("pgwire-{}", chrono::Utc::now().timestamp_millis()),
        resource_name: None,
        result,
        trace_id: None,
        secret_handle_refs: Vec::new(),
        metadata_redacted: metadata,
    };
    if let Err(e) = catalog.append_audit_event(&auth.tenant_id, Some(&auth.tenant_id), &event) {
        warn!("failed to append pgwire audit event: {e}");
    }
}

fn arrow_type_to_pg(dt: &DataType) -> Type {
    match dt {
        DataType::Boolean => Type::BOOL,
        DataType::Int8 | DataType::Int16 => Type::INT2,
        DataType::Int32 => Type::INT4,
        DataType::Int64 => Type::INT8,
        DataType::UInt8 | DataType::UInt16 => Type::INT2,
        DataType::UInt32 => Type::INT4,
        DataType::UInt64 => Type::INT8,
        DataType::Float32 => Type::FLOAT4,
        DataType::Float64 => Type::FLOAT8,
        DataType::Utf8 | DataType::LargeUtf8 => Type::VARCHAR,
        DataType::Date32 | DataType::Date64 => Type::DATE,
        DataType::Timestamp(_, _) => Type::TIMESTAMP,
        _ => Type::VARCHAR,
    }
}

fn schema_to_field_info(schema: &SchemaRef) -> Arc<Vec<FieldInfo>> {
    Arc::new(
        schema
            .fields()
            .iter()
            .map(|f| {
                FieldInfo::new(
                    f.name().clone(),
                    None,
                    None,
                    arrow_type_to_pg(f.data_type()),
                    FieldFormat::Text,
                )
            })
            .collect(),
    )
}

fn encode_batches(
    batches: Vec<RecordBatch>,
    fields: Arc<Vec<FieldInfo>>,
) -> Vec<PgWireResult<pgwire::messages::data::DataRow>> {
    let mut rows = Vec::new();
    for batch in &batches {
        for row_idx in 0..batch.num_rows() {
            let mut encoder = DataRowEncoder::new(fields.clone());
            for col_idx in 0..batch.num_columns() {
                let col = batch.column(col_idx);
                let val = arrow::util::display::array_value_to_string(col, row_idx)
                    .unwrap_or_else(|_| "NULL".to_string());
                if val == "NULL" || val.is_empty() {
                    encoder.encode_field(&None::<&str>).ok();
                } else {
                    encoder.encode_field(&val).ok();
                }
            }
            rows.push(encoder.finish());
        }
    }
    rows
}

#[async_trait]
impl SimpleQueryHandler for OpenSnowPgHandler {
    async fn do_query<'a, 'b: 'a, C>(
        &'b self,
        client: &mut C,
        query: &'a str,
    ) -> PgWireResult<Vec<Response<'a>>>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        tracing::info!("PG query: {}", query);
        let query = validate_demo_sql(query).map_err(|message| pg_user_error("0A000", message))?;

        let auth_context = if let Some(auth_state) = &self.auth {
            let raw = client
                .metadata()
                .get(PGWIRE_AUTH_CONTEXT_METADATA)
                .ok_or_else(|| {
                    pg_user_error(
                        "28000",
                        "pgwire connection is missing authenticated subject",
                    )
                })?;
            let ctx = deserialize_auth_context(raw).map_err(|e| pg_user_error("28000", e))?;
            if let Err(reason) = authorize_pgwire_sql(&auth_state.policy, &ctx, &query) {
                append_pgwire_audit(auth_state, &ctx, &query, AuditResult::Denied, Some(&reason));
                return Err(pg_user_error("42501", reason));
            }
            append_pgwire_audit(auth_state, &ctx, &query, AuditResult::Allowed, None);
            Some(ctx)
        } else {
            None
        };

        let warehouse = "default"; // Phase 1: all PG queries use default warehouse label
        let start = Instant::now();
        inc_active_queries();
        inc_warehouse_pending(warehouse);

        let result = self.handle.execute_sql(&query).await;

        dec_warehouse_pending(warehouse);
        dec_active_queries();

        match result {
            Ok(batches) => {
                let duration = start.elapsed();
                if batches.is_empty() {
                    record_query(duration, "success", 0);
                    let duration_ms = duration.as_millis() as i64;
                    if let Some(auth) = auth_context.as_ref() {
                        self.handle
                            .record_query_history_for_tenant(
                                &auth.tenant_id,
                                warehouse,
                                &query,
                                duration_ms,
                                0,
                                "success",
                            )
                            .await;
                    } else {
                        self.handle
                            .record_query_history(warehouse, &query, duration_ms, 0, "success")
                            .await;
                    }
                    return Ok(vec![Response::Execution(Tag::new("OK"))]);
                }

                let schema = batches[0].schema();
                let fields = schema_to_field_info(&schema);
                let total_rows: u64 = batches.iter().map(|b| b.num_rows() as u64).sum();
                record_query(duration, "success", total_rows);
                let duration_ms = duration.as_millis() as i64;
                if let Some(auth) = auth_context.as_ref() {
                    self.handle
                        .record_query_history_for_tenant(
                            &auth.tenant_id,
                            warehouse,
                            &query,
                            duration_ms,
                            total_rows as i64,
                            "success",
                        )
                        .await;
                } else {
                    self.handle
                        .record_query_history(
                            warehouse,
                            &query,
                            duration_ms,
                            total_rows as i64,
                            "success",
                        )
                        .await;
                }

                let encoded = encode_batches(batches, fields.clone());
                let data_row_stream = stream::iter(encoded);

                Ok(vec![Response::Query(QueryResponse::new(
                    fields,
                    data_row_stream.boxed(),
                ))])
            }
            Err(e) => {
                let duration = start.elapsed();
                record_query(duration, "error", 0);
                let duration_ms = duration.as_millis() as i64;
                if let Some(auth) = auth_context.as_ref() {
                    self.handle
                        .record_query_history_for_tenant(
                            &auth.tenant_id,
                            warehouse,
                            &query,
                            duration_ms,
                            0,
                            "error",
                        )
                        .await;
                } else {
                    self.handle
                        .record_query_history(warehouse, &query, duration_ms, 0, "error")
                        .await;
                }

                Err(PgWireError::UserError(Box::new(
                    pgwire::error::ErrorInfo::new(
                        "ERROR".to_string(),
                        "XX000".to_string(),
                        e.to_string(),
                    ),
                )))
            }
        }
    }
}

#[derive(Clone)]
pub struct OpenSnowStartupHandler {
    auth: Option<AuthState>,
    parameters: Arc<DefaultServerParameterProvider>,
}

impl OpenSnowStartupHandler {
    pub fn new(auth: Option<AuthState>) -> Self {
        Self {
            auth,
            parameters: Arc::new(DefaultServerParameterProvider::default()),
        }
    }
}

#[async_trait]
impl StartupHandler for OpenSnowStartupHandler {
    async fn on_startup<C>(
        &self,
        client: &mut C,
        message: PgWireFrontendMessage,
    ) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        match message {
            PgWireFrontendMessage::Startup(ref startup) => {
                auth::save_startup_parameters_to_metadata(client, startup);
                if self.auth.is_some() {
                    client.set_state(PgWireConnectionState::AuthenticationInProgress);
                    client
                        .send(PgWireBackendMessage::Authentication(
                            pgwire::messages::startup::Authentication::CleartextPassword,
                        ))
                        .await?;
                } else {
                    auth::finish_authentication(client, self.parameters.as_ref()).await?;
                }
            }
            PgWireFrontendMessage::PasswordMessageFamily(pwd) => {
                let Some(auth_state) = self.auth.as_ref() else {
                    return Ok(());
                };
                let pwd = pwd.into_password()?;
                let startup_user = client
                    .metadata()
                    .get(METADATA_USER)
                    .map(String::as_str)
                    .unwrap_or_default()
                    .to_string();
                let startup_database = client.metadata().get(METADATA_DATABASE).cloned();
                match authenticate_pgwire_bearer(
                    auth_state,
                    &startup_user,
                    startup_database.as_deref(),
                    &pwd.password,
                ) {
                    Ok(context) => {
                        client.metadata_mut().insert(
                            PGWIRE_AUTH_CONTEXT_METADATA.to_string(),
                            serialize_auth_context(&context),
                        );
                        auth::finish_authentication(client, self.parameters.as_ref()).await?;
                    }
                    Err(reason) => {
                        warn!(user = %startup_user, reason, "rejected pgwire startup authentication");
                        let error = ErrorResponse::from(ErrorInfo::new(
                            "FATAL".to_string(),
                            "28P01".to_string(),
                            "pgwire bearer authentication failed".to_string(),
                        ));
                        client
                            .feed(PgWireBackendMessage::ErrorResponse(error))
                            .await?;
                        client.close().await?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn unsupported_pgwire_feature_error(feature: &str) -> PgWireError {
    PgWireError::UserError(Box::new(pgwire::error::ErrorInfo::new(
        "ERROR".to_string(),
        "0A000".to_string(),
        format!(
            "OpenSnow trusted-local pgwire does not support {feature} yet. Use the simple query protocol (for example psql -c) for SELECT/SHOW/DESCRIBE smoke tests, use /api/v1/ingest for data loading instead of COPY, and see docs/SQL_COMPATIBILITY.md for the current client compatibility matrix."
        ),
    )))
}

#[derive(Debug, Clone)]
pub struct OpenSnowExtendedQueryHandler;

#[async_trait]
impl ExtendedQueryHandler for OpenSnowExtendedQueryHandler {
    type Statement = String;
    type QueryParser = NoopQueryParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        Arc::new(NoopQueryParser)
    }

    async fn do_describe_statement<C>(
        &self,
        _client: &mut C,
        _statement: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        Err(unsupported_pgwire_feature_error(
            "extended query protocol DESCRIBE",
        ))
    }

    async fn do_describe_portal<C>(
        &self,
        _client: &mut C,
        _portal: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        Err(unsupported_pgwire_feature_error(
            "extended query protocol DESCRIBE",
        ))
    }

    async fn do_query<'a, 'b: 'a, C>(
        &'b self,
        _client: &mut C,
        _portal: &'a Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response<'a>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        Err(unsupported_pgwire_feature_error("extended query protocol"))
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OpenSnowCopyHandler;

#[async_trait]
impl CopyHandler for OpenSnowCopyHandler {
    async fn on_copy_data<C>(&self, _client: &mut C, _copy_data: CopyData) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        Err(unsupported_pgwire_feature_error("COPY protocol"))
    }

    async fn on_copy_done<C>(&self, _client: &mut C, _done: CopyDone) -> PgWireResult<()>
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        Err(unsupported_pgwire_feature_error("COPY protocol"))
    }

    async fn on_copy_fail<C>(&self, _client: &mut C, _fail: CopyFail) -> PgWireError
    where
        C: ClientInfo + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        unsupported_pgwire_feature_error("COPY protocol")
    }
}

pub struct OpenSnowPgFactory {
    handler: Arc<OpenSnowPgHandler>,
    auth: Option<AuthState>,
}

impl OpenSnowPgFactory {
    pub fn new(handle: EngineHandle, auth: Option<AuthState>) -> Self {
        Self {
            handler: Arc::new(OpenSnowPgHandler::new(handle, auth.clone())),
            auth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user_error_fields(error: PgWireError) -> (String, String) {
        match error {
            PgWireError::UserError(info) => (info.code.clone(), info.message.clone()),
            other => panic!("expected user error, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_pgwire_feature_errors_are_clear_and_non_panicking() {
        let (code, message) =
            user_error_fields(unsupported_pgwire_feature_error("extended query protocol"));
        assert_eq!(code, "0A000");
        assert!(message.contains("extended query protocol"));
        assert!(message.contains("simple query protocol"));
        assert!(message.contains("docs/SQL_COMPATIBILITY.md"));

        let (code, message) = user_error_fields(unsupported_pgwire_feature_error("COPY protocol"));
        assert_eq!(code, "0A000");
        assert!(message.contains("COPY protocol"));
        assert!(message.contains("/api/v1/ingest"));
    }

    #[test]
    fn enterprise_pgwire_requires_a_valid_bearer_token_bound_to_startup_user_and_database() {
        let auth = crate::auth::AuthState::new(
            Arc::new(opensnow_auth::JwtManager::new(b"test-secret-key-12345")),
            crate::auth::ClientRegistry::new(),
            24,
        );
        let token = auth
            .jwt
            .generate_token_with_scopes(
                7,
                "alice@example.com",
                "ANALYST",
                "acct_acme",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();

        let ctx = authenticate_pgwire_bearer(&auth, "alice@example.com", Some("acct_acme"), &token)
            .expect("matching startup user/database and JWT should authenticate");
        assert_eq!(ctx.username, "alice@example.com");
        assert_eq!(ctx.tenant_id, "acct_acme");

        assert!(
            authenticate_pgwire_bearer(&auth, "mallory@example.com", Some("acct_acme"), &token)
                .is_err()
        );
        assert!(
            authenticate_pgwire_bearer(&auth, "alice@example.com", Some("acct_other"), &token)
                .is_err()
        );
        assert!(
            authenticate_pgwire_bearer(&auth, "alice@example.com", Some("acct_acme"), "not-a-jwt")
                .is_err()
        );

        let expired = auth
            .jwt
            .generate_token_with_scopes(
                7,
                "alice@example.com",
                "ANALYST",
                "acct_acme",
                vec!["sql.query".to_string(), "table.select".to_string()],
                -1,
            )
            .unwrap();
        assert!(
            authenticate_pgwire_bearer(&auth, "alice@example.com", Some("acct_acme"), &expired)
                .is_err()
        );

        let clients = crate::auth::ClientRegistry::new();
        clients.register_with_metadata(
            "svc-revoked",
            "not-used-for-jwt-auth",
            "ANALYST",
            "acct_acme",
            vec!["sql.query".to_string(), "table.select".to_string()],
        );
        clients
            .revoke_client("svc-revoked", "qa revoked identity")
            .unwrap();
        let revoked_auth = crate::auth::AuthState::new(auth.jwt.clone(), clients, 24);
        let revoked_token = revoked_auth
            .jwt
            .generate_token_with_scopes(
                8,
                "svc-revoked",
                "ANALYST",
                "acct_acme",
                vec!["sql.query".to_string(), "table.select".to_string()],
                1,
            )
            .unwrap();
        assert!(
            authenticate_pgwire_bearer(
                &revoked_auth,
                "svc-revoked",
                Some("acct_acme"),
                &revoked_token
            )
            .is_err()
        );
    }

    #[test]
    fn enterprise_pgwire_requires_query_scope_and_object_policy_before_execution() {
        let policy = crate::policy::ObjectPolicyStore::in_memory().unwrap();
        policy
            .grant_table_privilege(
                "ANALYST",
                opensnow_auth::Privilege::Select,
                "allowed_orders",
            )
            .unwrap();
        let ctx = crate::auth::AuthContext {
            user_id: 7,
            username: "alice@example.com".to_string(),
            role: "ANALYST".to_string(),
            tenant_id: "acct_acme".to_string(),
            scopes: vec!["sql.query".to_string(), "table.select".to_string()],
        };

        assert!(authorize_pgwire_sql(&policy, &ctx, "SELECT * FROM allowed_orders").is_ok());
        let denied =
            authorize_pgwire_sql(&policy, &ctx, "SELECT * FROM denied_orders").unwrap_err();
        assert!(denied.contains("object policy denied"));

        let no_query_scope = crate::auth::AuthContext {
            scopes: vec!["table.select".to_string()],
            ..ctx
        };
        let denied = authorize_pgwire_sql(&policy, &no_query_scope, "SELECT * FROM allowed_orders")
            .unwrap_err();
        assert!(denied.contains("sql.query"));
    }

    #[test]
    fn enterprise_pgwire_appends_allow_and_deny_audit_events_to_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("catalog.db");
        let catalog_path_str = catalog_path.to_str().unwrap();
        let mut auth = crate::auth::AuthState::new(
            Arc::new(opensnow_auth::JwtManager::new(b"test-secret-key-12345")),
            crate::auth::ClientRegistry::new(),
            24,
        )
        .with_durable_service_catalog_path(catalog_path_str);
        auth.policy =
            crate::policy::ObjectPolicyStore::from_catalog_path(catalog_path_str).unwrap();
        let ctx = crate::auth::AuthContext {
            user_id: 7,
            username: "alice@example.com".to_string(),
            role: "ANALYST".to_string(),
            tenant_id: "acct_acme".to_string(),
            scopes: vec!["sql.query".to_string(), "table.select".to_string()],
        };

        append_pgwire_audit(
            &auth,
            &ctx,
            "SELECT * FROM allowed_orders",
            AuditResult::Allowed,
            None,
        );
        append_pgwire_audit(
            &auth,
            &ctx,
            "SELECT * FROM denied_orders",
            AuditResult::Denied,
            Some("object policy denied"),
        );

        let catalog = Catalog::open(catalog_path_str).unwrap();
        let events = catalog
            .search_audit_events("acct_acme", Some("acct_acme"), 10)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.action == "sql.query"
            && event.event.get("result") == Some(&json!("Allowed"))));
        assert!(events.iter().any(|event| event.action == "sql.query"
            && event.event.get("result") == Some(&json!("Denied"))
            && event.event.pointer("/metadata_redacted/reason")
                == Some(&json!("object policy denied"))));
    }
}

impl PgWireServerHandlers for OpenSnowPgFactory {
    type StartupHandler = OpenSnowStartupHandler;
    type SimpleQueryHandler = OpenSnowPgHandler;
    type ExtendedQueryHandler = OpenSnowExtendedQueryHandler;
    type CopyHandler = OpenSnowCopyHandler;
    type ErrorHandler = NoopErrorHandler;

    fn simple_query_handler(&self) -> Arc<Self::SimpleQueryHandler> {
        self.handler.clone()
    }

    fn extended_query_handler(&self) -> Arc<Self::ExtendedQueryHandler> {
        Arc::new(OpenSnowExtendedQueryHandler)
    }

    fn startup_handler(&self) -> Arc<Self::StartupHandler> {
        Arc::new(OpenSnowStartupHandler::new(self.auth.clone()))
    }

    fn copy_handler(&self) -> Arc<Self::CopyHandler> {
        Arc::new(OpenSnowCopyHandler)
    }

    fn error_handler(&self) -> Arc<Self::ErrorHandler> {
        Arc::new(NoopErrorHandler)
    }
}
