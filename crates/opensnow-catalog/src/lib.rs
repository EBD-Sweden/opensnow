use anyhow::{Context, Result, bail};
use opensnow_auth::{
    AccountActivation, AuditEvent, AuditResult, EntitlementCheck, WarehouseActivation,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::info;

/// Convert a free-form tenant display name into a stable, lowercase id.
/// Allowed characters: `[a-z0-9_-]`. Whitespace and unsupported chars become
/// hyphens; consecutive hyphens collapse; leading/trailing hyphens are stripped.
fn slugify_tenant_id(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true; // suppress leading '-'
    for c in name.chars() {
        let lower = c.to_ascii_lowercase();
        if lower.is_ascii_alphanumeric() || lower == '_' {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Virtual warehouse metadata.
#[derive(Debug, Clone)]
pub struct Warehouse {
    pub id: i64,
    pub name: String,
    pub size: String,  // small|medium|large|xlarge
    pub state: String, // RUNNING|SUSPENDED
    pub min_nodes: i64,
    pub max_nodes: i64,
    pub auto_suspend_seconds: i64,
    pub account_id: Option<String>,
    pub organization_id: Option<String>,
}

/// Enterprise warehouse activation request.
#[derive(Debug, Clone, Copy)]
pub struct EnterpriseWarehouseRequest<'a> {
    pub account_id: &'a str,
    pub name: &'a str,
    pub size: &'a str,
    pub min_nodes: i64,
    pub max_nodes: i64,
    pub auto_suspend_seconds: i64,
}

#[derive(Debug, Clone, Copy)]
struct EnterpriseWarehouseRecord<'a> {
    account_id: &'a str,
    organization_id: &'a str,
    name: &'a str,
    size: &'a str,
    min_nodes: i64,
    max_nodes: i64,
    auto_suspend_seconds: i64,
}

#[derive(Debug)]
struct ActivationAuditInput<'a> {
    account_id: &'a str,
    organization_id: &'a str,
    action: &'a str,
    resource_type: &'a str,
    resource_id: &'a str,
    result: AuditResult,
    metadata_redacted: Map<String, Value>,
}

/// Recorded query execution metadata.
#[derive(Debug, Clone)]
pub struct QueryRecord {
    pub id: i64,
    pub submitted_at: String,
    pub user_name: Option<String>,
    pub warehouse: String,
    pub sql: String,
    pub duration_ms: i64,
    pub rows_returned: i64,
    pub rows_scanned: Option<i64>,
    pub status: String,
}

/// Input when inserting a new query record (without auto-generated fields).
#[derive(Debug, Clone)]
pub struct QueryRecordInput {
    pub user_name: Option<String>,
    pub warehouse: String,
    pub sql: String,
    pub duration_ms: i64,
    pub rows_returned: i64,
    pub rows_scanned: Option<i64>,
    pub status: String,
}

impl QueryRecordInput {
    /// Build a record with the default tenant.
    pub fn for_default_tenant(
        warehouse: &str,
        sql: &str,
        duration_ms: i64,
        rows: i64,
        status: &str,
    ) -> Self {
        Self {
            user_name: None,
            warehouse: warehouse.to_string(),
            sql: sql.to_string(),
            duration_ms,
            rows_returned: rows,
            rows_scanned: None,
            status: status.to_string(),
        }
    }
}

/// Materialized view metadata.
#[derive(Debug, Clone)]
pub struct MaterializedView {
    pub name: String,
    pub sql: String,
    pub last_refreshed: String,
    pub parquet_path: String,
}

/// Logical tenant for isolating warehouses, query history, and MVs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Tenant {
    pub id: String,
    pub name: String,
    pub created_at: String,
}

/// Default tenant id used when no `X-Tenant-ID` header is supplied.
pub const DEFAULT_TENANT: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub id: String,
    pub name: String,
    pub owner_email: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationRecord {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountWorkspace {
    pub id: String,
    pub account_id: String,
    pub organization_id: String,
    pub name: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountMembership {
    pub email: String,
    pub account_id: String,
    pub role: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBootstrap {
    pub account: AccountRecord,
    pub organization: OrganizationRecord,
    pub workspace: AccountWorkspace,
    pub owner_membership: AccountMembership,
    pub service_identity: ServiceIdentityClient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceIdentityClientInput {
    pub id: String,
    pub account_id: String,
    pub workspace_id: Option<String>,
    pub secret_hash: String,
    pub role: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceIdentityClient {
    pub id: String,
    pub account_id: String,
    pub workspace_id: Option<String>,
    pub secret_hash: String,
    pub role: String,
    pub scopes: Vec<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    pub rotated_at: Option<String>,
    pub revoked_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimTokenInput {
    pub id: String,
    pub account_id: String,
    pub label: String,
    pub secret_hash: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimTokenSnapshot {
    pub id: String,
    pub account_id: String,
    pub label: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimTokenRecord {
    pub id: String,
    pub account_id: String,
    pub label: String,
    pub created_at: String,
    pub revoked_at: Option<String>,
    pub secret_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimUserRecord {
    pub id: String,
    pub account_id: String,
    pub user_name: String,
    pub display_name: Option<String>,
    pub active: bool,
    pub lifecycle: String,
    pub external_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScimGroupRecord {
    pub id: String,
    pub account_id: String,
    pub display_name: String,
    pub role: String,
    pub members: Vec<String>,
    pub tombstoned: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogAuditEvent {
    pub id: i64,
    pub account_id: String,
    pub organization_id: Option<String>,
    pub action: String,
    pub event: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlPlaneResource {
    pub id: String,
    pub account_id: String,
    pub organization_id: Option<String>,
    pub workspace_id: Option<String>,
    pub resource_type: String,
    pub resource: Value,
    pub created_at: String,
    pub updated_at: String,
}

/// Embedded catalog backed by SQLite.
/// Stores table metadata, schemas, warehouse metadata, and user/role information.
pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    /// Current SQLite catalog schema version recorded in `catalog_migrations`.
    /// Increment when a migration changes persisted catalog shape.
    pub const SCHEMA_VERSION: i64 = 1;

    /// Open or create a catalog database at the given path.
    /// Use ":memory:" for in-memory catalog (tests / ephemeral mode).
    pub fn open(path: &str) -> Result<Self> {
        let conn = if path == ":memory:" {
            Connection::open_in_memory().context("failed to open in-memory catalog")?
        } else {
            let catalog_path = std::path::Path::new(path);
            if let Some(parent) = catalog_path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create catalog parent directory {}; check volume permissions or set [catalog].path to a writable location",
                        parent.display()
                    )
                })?;
            }
            Connection::open(catalog_path).with_context(|| {
                format!(
                    "failed to open catalog database {}; check file permissions or restore from backup before retrying",
                    catalog_path.display()
                )
            })?
        };

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("failed to configure SQLite catalog safety pragmas")?;

        let catalog = Self { conn };
        catalog.migrate()?;
        info!("Catalog opened at {}", path);
        Ok(catalog)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS catalog_migrations (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS tenants (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            INSERT OR IGNORE INTO tenants (id, name) VALUES ('default', 'Default tenant');

            CREATE TABLE IF NOT EXISTS databases (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS schemas (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                database_id INTEGER NOT NULL REFERENCES databases(id),
                name TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(database_id, name)
            );

            CREATE TABLE IF NOT EXISTS tables (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                schema_id INTEGER NOT NULL REFERENCES schemas(id),
                name TEXT NOT NULL,
                table_type TEXT NOT NULL DEFAULT 'BASE TABLE',
                location TEXT,
                file_format TEXT NOT NULL DEFAULT 'parquet',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(schema_id, name)
            );

            -- Seed default database and schema
            INSERT OR IGNORE INTO databases (name) VALUES ('opensnow');
            INSERT OR IGNORE INTO schemas (database_id, name)
                VALUES ((SELECT id FROM databases WHERE name = 'opensnow'), 'public');

            -- Virtual warehouses (Phase 1: metadata only, no K8s orchestration)
            CREATE TABLE IF NOT EXISTS warehouses (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                size TEXT NOT NULL DEFAULT 'small',
                state TEXT NOT NULL DEFAULT 'SUSPENDED',
                min_nodes INTEGER NOT NULL DEFAULT 0,
                max_nodes INTEGER NOT NULL DEFAULT 4,
                auto_suspend_seconds INTEGER NOT NULL DEFAULT 300,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            INSERT OR IGNORE INTO warehouses (name) VALUES ('default');

            -- Query history (lightweight audit of executed statements)
            CREATE TABLE IF NOT EXISTS query_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                tenant_id TEXT NOT NULL DEFAULT 'default',
                submitted_at TEXT NOT NULL DEFAULT (datetime('now')),
                user_name TEXT,
                warehouse TEXT NOT NULL,
                sql TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                rows_returned INTEGER NOT NULL,
                rows_scanned INTEGER,
                status TEXT NOT NULL  -- 'success' | 'error'
            );

            CREATE INDEX IF NOT EXISTS idx_query_history_time
                ON query_history(submitted_at);

            -- Materialized views (precomputed query results stored as Parquet)
            CREATE TABLE IF NOT EXISTS materialized_views (
                tenant_id TEXT NOT NULL DEFAULT 'default',
                name TEXT NOT NULL,
                sql TEXT NOT NULL,
                last_refreshed TEXT NOT NULL,
                parquet_path TEXT NOT NULL,
                PRIMARY KEY (tenant_id, name)
            );

            CREATE TABLE IF NOT EXISTS account_bootstrap_records (
                account_id TEXT PRIMARY KEY,
                bootstrap_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS service_identities (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                workspace_id TEXT,
                secret_hash TEXT NOT NULL,
                role TEXT NOT NULL,
                scopes_json TEXT NOT NULL,
                status TEXT NOT NULL,
                status_reason TEXT,
                expires_at TEXT,
                last_used_at TEXT,
                rotated_at TEXT,
                revoked_at TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scim_tokens (
                id TEXT PRIMARY KEY,
                account_id TEXT NOT NULL,
                label TEXT NOT NULL,
                secret_hash TEXT NOT NULL,
                created_at TEXT NOT NULL,
                revoked_at TEXT
            );

            CREATE TABLE IF NOT EXISTS scim_users (
                id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                user_name TEXT NOT NULL,
                display_name TEXT,
                active INTEGER NOT NULL,
                lifecycle TEXT NOT NULL,
                external_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (account_id, id)
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_scim_users_account_name ON scim_users(account_id, user_name);

            CREATE TABLE IF NOT EXISTS scim_groups (
                id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                display_name TEXT NOT NULL,
                role TEXT NOT NULL,
                members_json TEXT NOT NULL,
                tombstoned INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (account_id, id)
            );

            CREATE TABLE IF NOT EXISTS audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                account_id TEXT NOT NULL,
                organization_id TEXT,
                event_json TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS control_plane_resources (
                resource_type TEXT NOT NULL,
                id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                organization_id TEXT,
                workspace_id TEXT,
                resource_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (resource_type, id)
            );
            CREATE INDEX IF NOT EXISTS idx_control_plane_org ON control_plane_resources(organization_id, resource_type);
            CREATE INDEX IF NOT EXISTS idx_control_plane_workspace ON control_plane_resources(workspace_id, resource_type);
            ",
        )?;

        // Backfill columns on databases that were created before multi-tenancy.
        // Both tables guarantee `tenant_id` once these are applied; missing
        // column means the schema predates multi-tenancy and we add it.
        Self::ensure_column(
            &self.conn,
            "query_history",
            "tenant_id",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        Self::ensure_column(
            &self.conn,
            "materialized_views",
            "tenant_id",
            "TEXT NOT NULL DEFAULT 'default'",
        )?;
        Self::ensure_column(&self.conn, "warehouses", "account_id", "TEXT")?;
        Self::ensure_column(&self.conn, "warehouses", "organization_id", "TEXT")?;

        // Indexes that depend on backfilled columns have to come *after*
        // ensure_column — pre-existing DBs may not have the columns when the
        // CREATE-TABLE batch above runs.
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_query_history_tenant ON query_history(tenant_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_warehouses_account_org ON warehouses(account_id, organization_id)",
            [],
        )?;

        // Legacy catalogs had `materialized_views.name` as the only primary
        // key. The current upsert path targets `(tenant_id, name)`, so ensure a
        // matching uniqueness guarantee exists when upgrading in place.
        self.conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_materialized_views_tenant_name
                ON materialized_views(tenant_id, name)",
            [],
        )?;

        self.conn.execute(
            "INSERT INTO catalog_migrations (id, version, applied_at)
             VALUES (1, ?1, datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
                version = excluded.version,
                applied_at = excluded.applied_at",
            rusqlite::params![Self::SCHEMA_VERSION],
        )?;

        info!("Catalog migrations applied");
        Ok(())
    }

    /// Reset ephemeral runtime state while preserving durable catalog metadata.
    /// This is safe for external demos: registered tables, sample-data file
    /// locations, tenants, and warehouse definitions remain intact while query
    /// history and materialized-view cache metadata are cleared.
    pub fn reset_runtime_state(&self) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM query_history", [])?;
        tx.execute("DELETE FROM materialized_views", [])?;
        tx.commit()?;
        info!("Catalog runtime state reset (query_history and materialized_views cleared)");
        Ok(())
    }

    /// Add a column to a table if it does not already exist. Used to upgrade
    /// pre-existing catalog DBs to the current schema.
    fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        if !cols.iter().any(|c| c == column) {
            conn.execute(
                &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
                [],
            )?;
        }
        Ok(())
    }

    pub fn register_table(
        &self,
        database: &str,
        schema: &str,
        table: &str,
        location: &str,
    ) -> Result<()> {
        let schema_id: i64 = self.conn.query_row(
            "SELECT s.id FROM schemas s JOIN databases d ON s.database_id = d.id WHERE d.name = ?1 AND s.name = ?2",
            rusqlite::params![database, schema],
            |row| row.get(0),
        )?;

        self.conn.execute(
            "INSERT OR REPLACE INTO tables (schema_id, name, location) VALUES (?1, ?2, ?3)",
            rusqlite::params![schema_id, table, location],
        )?;

        info!(
            "Registered table: {}.{}.{} -> {}",
            database, schema, table, location
        );
        Ok(())
    }

    // ── Tenant API ────────────────────────────────────────────────────────

    /// List all known tenants.
    pub fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM tenants ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Tenant {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        let mut tenants = Vec::new();
        for r in rows {
            tenants.push(r?);
        }
        Ok(tenants)
    }

    /// Look up a tenant by id.
    pub fn get_tenant(&self, id: &str) -> Result<Option<Tenant>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, created_at FROM tenants WHERE id = ?1")?;
        let mut rows = stmt.query_map(rusqlite::params![id], |row| {
            Ok(Tenant {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        match rows.next() {
            Some(Ok(t)) => Ok(Some(t)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Create a new tenant. The tenant id is derived from the display name by
    /// lowercasing and replacing whitespace with hyphens.
    pub fn create_tenant(&self, name: &str) -> Result<Tenant> {
        let id = slugify_tenant_id(name);
        if id.is_empty() {
            anyhow::bail!("tenant name produces an empty id");
        }
        self.conn.execute(
            "INSERT INTO tenants (id, name) VALUES (?1, ?2)",
            rusqlite::params![id, name],
        )?;
        let tenant = self
            .get_tenant(&id)?
            .ok_or_else(|| anyhow::anyhow!("tenant not found after insert"))?;
        info!("Created tenant: {} ({})", tenant.id, tenant.name);
        Ok(tenant)
    }

    // ── Query history API ─────────────────────────────────────────────────

    /// Insert a new query execution record under the default tenant.
    pub fn insert_query_record(&self, record: &QueryRecordInput) -> Result<()> {
        self.insert_query_record_for_tenant(DEFAULT_TENANT, record)
    }

    /// Insert a new query execution record scoped to `tenant_id`.
    pub fn insert_query_record_for_tenant(
        &self,
        tenant_id: &str,
        record: &QueryRecordInput,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO query_history (tenant_id, user_name, warehouse, sql, duration_ms, rows_returned, rows_scanned, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                tenant_id,
                record.user_name,
                record.warehouse,
                record.sql,
                record.duration_ms,
                record.rows_returned,
                record.rows_scanned,
                record.status,
            ],
        )?;
        Ok(())
    }

    /// Fetch the most recent query records (across all tenants) ordered by newest first.
    pub fn recent_queries(&self, limit: usize) -> Result<Vec<QueryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, submitted_at, user_name, warehouse, sql, duration_ms, rows_returned, rows_scanned, status
             FROM query_history
             ORDER BY id DESC
             LIMIT ?1",
        )?;

        let rows = stmt.query_map(rusqlite::params![limit as i64], |row| {
            Ok(QueryRecord {
                id: row.get(0)?,
                submitted_at: row.get(1)?,
                user_name: row.get(2)?,
                warehouse: row.get(3)?,
                sql: row.get(4)?,
                duration_ms: row.get(5)?,
                rows_returned: row.get(6)?,
                rows_scanned: row.get(7)?,
                status: row.get(8)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    /// Fetch the most recent query records scoped to a tenant.
    pub fn recent_queries_for_tenant(
        &self,
        tenant_id: &str,
        limit: usize,
    ) -> Result<Vec<QueryRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, submitted_at, user_name, warehouse, sql, duration_ms, rows_returned, rows_scanned, status
             FROM query_history
             WHERE tenant_id = ?1
             ORDER BY id DESC
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![tenant_id, limit as i64], |row| {
            Ok(QueryRecord {
                id: row.get(0)?,
                submitted_at: row.get(1)?,
                user_name: row.get(2)?,
                warehouse: row.get(3)?,
                sql: row.get(4)?,
                duration_ms: row.get(5)?,
                rows_returned: row.get(6)?,
                rows_scanned: row.get(7)?,
                status: row.get(8)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    // ── Warehouse API ──────────────────────────────────────────────────────

    /// List all virtual warehouses.
    pub fn list_warehouses(&self) -> Result<Vec<Warehouse>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, size, state, min_nodes, max_nodes, auto_suspend_seconds, account_id, organization_id FROM warehouses ORDER BY name"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Warehouse {
                id: row.get(0)?,
                name: row.get(1)?,
                size: row.get(2)?,
                state: row.get(3)?,
                min_nodes: row.get(4)?,
                max_nodes: row.get(5)?,
                auto_suspend_seconds: row.get(6)?,
                account_id: row.get(7)?,
                organization_id: row.get(8)?,
            })
        })?;
        let mut warehouses = Vec::new();
        for row in rows {
            warehouses.push(row?);
        }
        Ok(warehouses)
    }

    /// Get a warehouse by name.
    pub fn get_warehouse(&self, name: &str) -> Result<Option<Warehouse>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, size, state, min_nodes, max_nodes, auto_suspend_seconds, account_id, organization_id FROM warehouses WHERE name = ?1"
        )?;
        let mut rows = stmt.query_map(rusqlite::params![name], |row| {
            Ok(Warehouse {
                id: row.get(0)?,
                name: row.get(1)?,
                size: row.get(2)?,
                state: row.get(3)?,
                min_nodes: row.get(4)?,
                max_nodes: row.get(5)?,
                auto_suspend_seconds: row.get(6)?,
                account_id: row.get(7)?,
                organization_id: row.get(8)?,
            })
        })?;
        match rows.next() {
            Some(Ok(wh)) => Ok(Some(wh)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Create a new virtual warehouse.
    pub fn create_warehouse(
        &self,
        name: &str,
        size: &str,
        min_nodes: i64,
        max_nodes: i64,
        auto_suspend_seconds: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO warehouses (name, size, min_nodes, max_nodes, auto_suspend_seconds) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![name, size, min_nodes, max_nodes, auto_suspend_seconds],
        )?;
        info!(
            "Created warehouse: {} (size={}, min_nodes={}, max_nodes={}, auto_suspend={}s)",
            name, size, min_nodes, max_nodes, auto_suspend_seconds
        );
        Ok(())
    }

    fn count_enterprise_warehouses(
        &self,
        account_id: &str,
        organization_id: &str,
    ) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM warehouses WHERE account_id = ?1 AND organization_id = ?2",
            rusqlite::params![account_id, organization_id],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    fn create_enterprise_warehouse_record(
        &self,
        request: EnterpriseWarehouseRecord<'_>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO warehouses (name, size, min_nodes, max_nodes, auto_suspend_seconds, account_id, organization_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                request.name,
                request.size,
                request.min_nodes,
                request.max_nodes,
                request.auto_suspend_seconds,
                request.account_id,
                request.organization_id
            ],
        )?;
        info!(
            "Created enterprise warehouse: {} (account_id={}, organization_id={}, size={}, min_nodes={}, max_nodes={}, auto_suspend={}s)",
            request.name,
            request.account_id,
            request.organization_id,
            request.size,
            request.min_nodes,
            request.max_nodes,
            request.auto_suspend_seconds
        );
        Ok(())
    }

    pub fn create_enterprise_warehouse(
        &self,
        request: EnterpriseWarehouseRequest<'_>,
        entitlement: Option<&EntitlementCheck>,
    ) -> Result<()> {
        let organization_id = format!("org_{}", request.account_id);
        let current_enterprise_warehouses =
            self.count_enterprise_warehouses(request.account_id, &organization_id)?;
        let activation = WarehouseActivation::new(
            &organization_id,
            request.account_id,
            request.name,
            current_enterprise_warehouses + 1,
        );
        let allowed = entitlement
            .map(|entitlement| activation.is_allowed(entitlement))
            .unwrap_or(false);
        if !allowed {
            let reason = Self::entitlement_denial_reason(
                entitlement,
                &organization_id,
                "warehouse.activate",
            );
            self.append_activation_audit(ActivationAuditInput {
                account_id: request.account_id,
                organization_id: &organization_id,
                action: "warehouse.activate",
                resource_type: "warehouse",
                resource_id: request.name,
                result: AuditResult::Denied,
                metadata_redacted: Self::entitlement_metadata(Some(reason), entitlement),
            })?;
            bail!("enterprise warehouse activation denied: {reason}");
        }
        self.create_enterprise_warehouse_record(EnterpriseWarehouseRecord {
            account_id: request.account_id,
            organization_id: &organization_id,
            name: request.name,
            size: request.size,
            min_nodes: request.min_nodes,
            max_nodes: request.max_nodes,
            auto_suspend_seconds: request.auto_suspend_seconds,
        })?;
        self.append_activation_audit(ActivationAuditInput {
            account_id: request.account_id,
            organization_id: &organization_id,
            action: "warehouse.activate",
            resource_type: "warehouse",
            resource_id: request.name,
            result: AuditResult::Allowed,
            metadata_redacted: Self::entitlement_metadata(None, entitlement),
        })?;
        Ok(())
    }

    /// Update warehouse state (RUNNING or SUSPENDED).
    pub fn update_warehouse_state(&self, name: &str, state: &str) -> Result<()> {
        let rows = self.conn.execute(
            "UPDATE warehouses SET state = ?1 WHERE name = ?2",
            rusqlite::params![state, name],
        )?;
        if rows == 0 {
            anyhow::bail!("Warehouse '{}' not found", name);
        }
        info!("Updated warehouse '{}' state to {}", name, state);
        Ok(())
    }

    // ── Table API ────────────────────────────────────────────────────────

    // ── Materialized view API ────────────────────────────────────────────
    //
    // All MV methods come in two flavours: the bare name acts on the default
    // tenant (kept for backwards compatibility with the engine & existing
    // tests) and the `_for_tenant` variant scopes by `tenant_id`.

    pub fn upsert_materialized_view(
        &self,
        name: &str,
        sql: &str,
        parquet_path: &str,
    ) -> Result<()> {
        self.upsert_materialized_view_for_tenant(DEFAULT_TENANT, name, sql, parquet_path)
    }

    pub fn upsert_materialized_view_for_tenant(
        &self,
        tenant_id: &str,
        name: &str,
        sql: &str,
        parquet_path: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO materialized_views (tenant_id, name, sql, last_refreshed, parquet_path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(tenant_id, name) DO UPDATE SET
                 sql = excluded.sql,
                 last_refreshed = excluded.last_refreshed,
                 parquet_path = excluded.parquet_path",
            rusqlite::params![tenant_id, name, sql, now, parquet_path],
        )?;
        info!(
            "Upserted materialized view: {}.{} -> {}",
            tenant_id, name, parquet_path
        );
        Ok(())
    }

    pub fn touch_materialized_view(&self, name: &str) -> Result<()> {
        self.touch_materialized_view_for_tenant(DEFAULT_TENANT, name)
    }

    pub fn touch_materialized_view_for_tenant(&self, tenant_id: &str, name: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE materialized_views SET last_refreshed = ?1 WHERE tenant_id = ?2 AND name = ?3",
            rusqlite::params![now, tenant_id, name],
        )?;
        if rows == 0 {
            anyhow::bail!(
                "Materialized view '{}' not found in tenant '{}'",
                name,
                tenant_id
            );
        }
        Ok(())
    }

    pub fn get_materialized_view(&self, name: &str) -> Result<Option<MaterializedView>> {
        self.get_materialized_view_for_tenant(DEFAULT_TENANT, name)
    }

    pub fn get_materialized_view_for_tenant(
        &self,
        tenant_id: &str,
        name: &str,
    ) -> Result<Option<MaterializedView>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, sql, last_refreshed, parquet_path
               FROM materialized_views
              WHERE tenant_id = ?1 AND name = ?2",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![tenant_id, name], |row| {
            Ok(MaterializedView {
                name: row.get(0)?,
                sql: row.get(1)?,
                last_refreshed: row.get(2)?,
                parquet_path: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(Ok(mv)) => Ok(Some(mv)),
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    pub fn list_materialized_views(&self) -> Result<Vec<MaterializedView>> {
        self.list_materialized_views_for_tenant(DEFAULT_TENANT)
    }

    pub fn list_materialized_views_for_tenant(
        &self,
        tenant_id: &str,
    ) -> Result<Vec<MaterializedView>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, sql, last_refreshed, parquet_path
               FROM materialized_views
              WHERE tenant_id = ?1
           ORDER BY name",
        )?;
        let rows = stmt.query_map(rusqlite::params![tenant_id], |row| {
            Ok(MaterializedView {
                name: row.get(0)?,
                sql: row.get(1)?,
                last_refreshed: row.get(2)?,
                parquet_path: row.get(3)?,
            })
        })?;
        let mut views = Vec::new();
        for row in rows {
            views.push(row?);
        }
        Ok(views)
    }

    pub fn delete_materialized_view(&self, name: &str) -> Result<bool> {
        self.delete_materialized_view_for_tenant(DEFAULT_TENANT, name)
    }

    pub fn delete_materialized_view_for_tenant(&self, tenant_id: &str, name: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM materialized_views WHERE tenant_id = ?1 AND name = ?2",
            rusqlite::params![tenant_id, name],
        )?;
        Ok(rows > 0)
    }

    pub fn list_tables(&self, database: &str, schema: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.name, COALESCE(t.location, '') FROM tables t
             JOIN schemas s ON t.schema_id = s.id
             JOIN databases d ON s.database_id = d.id
             WHERE d.name = ?1 AND s.name = ?2",
        )?;

        let rows = stmt.query_map(rusqlite::params![database, schema], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut tables = Vec::new();
        for row in rows {
            tables.push(row?);
        }
        Ok(tables)
    }
}

impl Catalog {
    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn account_id_from_name(name: &str) -> String {
        let slug = slugify_tenant_id(name);
        if slug.is_empty() {
            "account".to_string()
        } else {
            slug
        }
    }

    fn entitlement_denial_reason(
        entitlement: Option<&EntitlementCheck>,
        expected_org: &str,
        required_feature: &str,
    ) -> &'static str {
        let Some(entitlement) = entitlement else {
            return "missing_entitlement";
        };
        if !entitlement.is_active() {
            return "inactive_entitlement";
        }
        if !entitlement.has_feature(required_feature) {
            return "missing_feature";
        }
        if entitlement.marketplace_identity.organization_id != expected_org {
            return "identity_mismatch";
        }
        "denied"
    }

    fn entitlement_metadata(
        reason: Option<&str>,
        entitlement: Option<&EntitlementCheck>,
    ) -> Map<String, Value> {
        let mut metadata = Map::new();
        if let Some(reason) = reason {
            metadata.insert("reason".to_string(), Value::String(reason.to_string()));
        }
        if let Some(entitlement) = entitlement {
            metadata.insert(
                "provider".to_string(),
                Value::String(
                    entitlement
                        .marketplace_identity
                        .provider
                        .as_str()
                        .to_string(),
                ),
            );
            metadata.insert(
                "external_customer_id".to_string(),
                Value::String(
                    entitlement
                        .marketplace_identity
                        .external_customer_id
                        .clone(),
                ),
            );
            metadata.insert(
                "product_code".to_string(),
                Value::String(entitlement.marketplace_identity.product_code.clone()),
            );
            metadata.insert(
                "entitlement_id".to_string(),
                Value::String(entitlement.marketplace_identity.entitlement_id.clone()),
            );
        }
        metadata
    }

    fn append_activation_audit(&self, input: ActivationAuditInput<'_>) -> Result<i64> {
        let event = AuditEvent {
            event_time: chrono::Utc::now(),
            organization_id: input.organization_id.to_string(),
            tenant_id: Some(input.account_id.to_string()),
            actor_type: "marketplace".to_string(),
            actor_id: "entitlement-gate".to_string(),
            actor_display: None,
            actor_auth_method: Some("marketplace_entitlement".to_string()),
            action: input.action.to_string(),
            resource_type: input.resource_type.to_string(),
            resource_id: input.resource_id.to_string(),
            resource_name: None,
            result: input.result,
            trace_id: None,
            secret_handle_refs: Vec::new(),
            metadata_redacted: input.metadata_redacted,
        };
        self.append_audit_event(input.account_id, Some(input.organization_id), &event)
    }

    pub fn register_enterprise_account(
        &self,
        account_name: &str,
        owner_email: &str,
        entitlement: Option<&EntitlementCheck>,
    ) -> Result<AccountBootstrap> {
        let account_id = Self::account_id_from_name(account_name);
        let organization_id = format!("org_{account_id}");
        if !owner_email.contains('@') {
            let reason = "owner_email_unverified";
            self.append_activation_audit(ActivationAuditInput {
                account_id: &account_id,
                organization_id: &organization_id,
                action: "account.activate",
                resource_type: "account",
                resource_id: &account_id,
                result: AuditResult::Denied,
                metadata_redacted: Self::entitlement_metadata(Some(reason), entitlement),
            })?;
            bail!("enterprise account activation denied: {reason}");
        }
        let activation = AccountActivation::new(&organization_id, &account_id);
        let allowed = entitlement
            .map(|entitlement| activation.is_allowed(entitlement))
            .unwrap_or(false);
        if !allowed {
            let reason =
                Self::entitlement_denial_reason(entitlement, &organization_id, "account.activate");
            self.append_activation_audit(ActivationAuditInput {
                account_id: &account_id,
                organization_id: &organization_id,
                action: "account.activate",
                resource_type: "account",
                resource_id: &account_id,
                result: AuditResult::Denied,
                metadata_redacted: Self::entitlement_metadata(Some(reason), entitlement),
            })?;
            bail!("enterprise account activation denied: {reason}");
        }
        let bootstrap = self.register_account(account_name, owner_email)?;
        self.append_activation_audit(ActivationAuditInput {
            account_id: &account_id,
            organization_id: &organization_id,
            action: "account.activate",
            resource_type: "account",
            resource_id: &account_id,
            result: AuditResult::Allowed,
            metadata_redacted: Self::entitlement_metadata(None, entitlement),
        })?;
        Ok(bootstrap)
    }

    pub fn register_account(
        &self,
        account_name: &str,
        owner_email: &str,
    ) -> Result<AccountBootstrap> {
        let now = Self::now_rfc3339();
        let account_id = Self::account_id_from_name(account_name);
        let organization_id = format!("org_{account_id}");
        let workspace_id = format!("ws_{account_id}_default");
        let service_identity_id = format!("svc_{account_id}_bootstrap");
        let account = AccountRecord {
            id: account_id.clone(),
            name: account_name.to_string(),
            owner_email: owner_email.to_string(),
            created_at: now.clone(),
        };
        let organization = OrganizationRecord {
            id: organization_id.clone(),
            account_id: account_id.clone(),
            name: account_name.to_string(),
            created_at: now.clone(),
        };
        let workspace = AccountWorkspace {
            id: workspace_id.clone(),
            account_id: account_id.clone(),
            organization_id: organization_id.clone(),
            name: "default".to_string(),
            created_at: now.clone(),
        };
        let owner_membership = AccountMembership {
            email: owner_email.to_string(),
            account_id: account_id.clone(),
            role: "ACCOUNTOWNER".to_string(),
            created_at: now.clone(),
        };
        let service_identity = ServiceIdentityClient {
            id: service_identity_id,
            account_id: account_id.clone(),
            workspace_id: Some(workspace_id),
            secret_hash: String::new(),
            role: "ACCOUNTADMIN".to_string(),
            scopes: vec!["*".to_string()],
            status: "active".to_string(),
            status_reason: None,
            expires_at: None,
            last_used_at: None,
            rotated_at: None,
            revoked_at: None,
            created_at: now,
        };
        let bootstrap = AccountBootstrap {
            account,
            organization,
            workspace,
            owner_membership,
            service_identity,
        };
        self.conn.execute(
            "INSERT OR REPLACE INTO account_bootstrap_records (account_id, bootstrap_json, created_at) VALUES (?1, ?2, datetime('now'))",
            params![account_id, serde_json::to_string(&bootstrap)?],
        )?;
        Ok(bootstrap)
    }

    pub fn create_workspace_for_account(
        &self,
        actor_account_id: &str,
        account_id: &str,
        name: &str,
    ) -> Result<AccountWorkspace> {
        if actor_account_id != account_id {
            bail!("actor account cannot mutate a different account");
        }
        let now = Self::now_rfc3339();
        let workspace = AccountWorkspace {
            id: format!("ws_{}_{}", account_id, slugify_tenant_id(name)),
            account_id: account_id.to_string(),
            organization_id: format!("org_{account_id}"),
            name: name.to_string(),
            created_at: now,
        };
        Ok(workspace)
    }

    fn control_plane_resource_from_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ControlPlaneResource> {
        let resource_json: String = row.get(5)?;
        Ok(ControlPlaneResource {
            resource_type: row.get(0)?,
            id: row.get(1)?,
            account_id: row.get(2)?,
            organization_id: row.get(3)?,
            workspace_id: row.get(4)?,
            resource: serde_json::from_str(&resource_json).unwrap_or(Value::Null),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    pub fn upsert_control_plane_resource(
        &self,
        resource_type: &str,
        id: &str,
        account_id: &str,
        organization_id: Option<&str>,
        workspace_id: Option<&str>,
        resource: &Value,
    ) -> Result<ControlPlaneResource> {
        let now = Self::now_rfc3339();
        self.conn.execute(
            "INSERT INTO control_plane_resources (resource_type, id, account_id, organization_id, workspace_id, resource_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(resource_type, id) DO UPDATE SET
                account_id = excluded.account_id,
                organization_id = excluded.organization_id,
                workspace_id = excluded.workspace_id,
                resource_json = excluded.resource_json,
                updated_at = excluded.updated_at",
            params![resource_type, id, account_id, organization_id, workspace_id, serde_json::to_string(resource)?, now],
        )?;
        self.get_control_plane_resource(resource_type, id)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "control-plane resource disappeared after upsert: {resource_type}/{id}"
                )
            })
    }

    pub fn get_control_plane_resource(
        &self,
        resource_type: &str,
        id: &str,
    ) -> Result<Option<ControlPlaneResource>> {
        self.conn.query_row(
            "SELECT resource_type, id, account_id, organization_id, workspace_id, resource_json, created_at, updated_at FROM control_plane_resources WHERE resource_type = ?1 AND id = ?2",
            params![resource_type, id],
            Self::control_plane_resource_from_row,
        ).optional().map_err(Into::into)
    }

    pub fn list_control_plane_resources(
        &self,
        account_id: &str,
        organization_id: Option<&str>,
        workspace_id: Option<&str>,
        resource_type: Option<&str>,
    ) -> Result<Vec<ControlPlaneResource>> {
        let mut sql = "SELECT resource_type, id, account_id, organization_id, workspace_id, resource_json, created_at, updated_at FROM control_plane_resources WHERE account_id = ?".to_string();
        let mut values = vec![account_id.to_string()];
        if let Some(organization_id) = organization_id {
            sql.push_str(" AND organization_id = ?");
            values.push(organization_id.to_string());
        }
        if let Some(workspace_id) = workspace_id {
            sql.push_str(" AND workspace_id = ?");
            values.push(workspace_id.to_string());
        }
        if let Some(resource_type) = resource_type {
            sql.push_str(" AND resource_type = ?");
            values.push(resource_type.to_string());
        }
        sql.push_str(" ORDER BY resource_type, id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(values),
            Self::control_plane_resource_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn service_client_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceIdentityClient> {
        let scopes_json: String = row.get(5)?;
        Ok(ServiceIdentityClient {
            id: row.get(0)?,
            account_id: row.get(1)?,
            workspace_id: row.get(2)?,
            secret_hash: row.get(3)?,
            role: row.get(4)?,
            scopes: serde_json::from_str(&scopes_json).unwrap_or_default(),
            status: row.get(6)?,
            status_reason: row.get(7)?,
            expires_at: row.get(8)?,
            last_used_at: row.get(9)?,
            rotated_at: row.get(10)?,
            revoked_at: row.get(11)?,
            created_at: row.get(12)?,
        })
    }

    pub fn create_service_identity_client(
        &self,
        input: ServiceIdentityClientInput,
    ) -> Result<ServiceIdentityClient> {
        let now = Self::now_rfc3339();
        let record = ServiceIdentityClient {
            id: input.id,
            account_id: input.account_id,
            workspace_id: input.workspace_id,
            secret_hash: input.secret_hash,
            role: input.role,
            scopes: input.scopes,
            status: "active".to_string(),
            status_reason: None,
            expires_at: input.expires_at,
            last_used_at: None,
            rotated_at: None,
            revoked_at: None,
            created_at: now,
        };
        self.conn.execute(
            "INSERT INTO service_identities (id, account_id, workspace_id, secret_hash, role, scopes_json, status, status_reason, expires_at, last_used_at, rotated_at, revoked_at, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![record.id, record.account_id, record.workspace_id, record.secret_hash, record.role, serde_json::to_string(&record.scopes)?, record.status, record.status_reason, record.expires_at, record.last_used_at, record.rotated_at, record.revoked_at, record.created_at],
        )?;
        Ok(record)
    }

    pub fn get_service_identity_client(&self, id: &str) -> Result<Option<ServiceIdentityClient>> {
        self.conn.query_row(
            "SELECT id, account_id, workspace_id, secret_hash, role, scopes_json, status, status_reason, expires_at, last_used_at, rotated_at, revoked_at, created_at FROM service_identities WHERE id = ?1",
            params![id], Self::service_client_from_row,
        ).optional().map_err(Into::into)
    }

    pub fn list_service_identity_clients(
        &self,
        account_id: Option<&str>,
    ) -> Result<Vec<ServiceIdentityClient>> {
        let sql_all = "SELECT id, account_id, workspace_id, secret_hash, role, scopes_json, status, status_reason, expires_at, last_used_at, rotated_at, revoked_at, created_at FROM service_identities ORDER BY id";
        let sql_account = "SELECT id, account_id, workspace_id, secret_hash, role, scopes_json, status, status_reason, expires_at, last_used_at, rotated_at, revoked_at, created_at FROM service_identities WHERE account_id = ?1 ORDER BY id";
        let mut stmt = self.conn.prepare(if account_id.is_some() {
            sql_account
        } else {
            sql_all
        })?;
        let rows = match account_id {
            Some(account_id) => stmt
                .query_map(params![account_id], Self::service_client_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map([], Self::service_client_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn mark_service_identity_used(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE service_identities SET last_used_at = ?2 WHERE id = ?1",
            params![id, Self::now_rfc3339()],
        )?;
        Ok(())
    }

    pub fn rotate_service_identity_secret(&self, id: &str, secret_hash: &str) -> Result<()> {
        let changed = self.conn.execute("UPDATE service_identities SET secret_hash = ?2, rotated_at = ?3, status = 'active', revoked_at = NULL, status_reason = NULL WHERE id = ?1", params![id, secret_hash, Self::now_rfc3339()])?;
        if changed == 0 {
            bail!("service identity client not found: {id}");
        }
        Ok(())
    }

    pub fn revoke_service_identity_client(&self, id: &str, reason: &str) -> Result<()> {
        let changed = self.conn.execute("UPDATE service_identities SET status = 'revoked', status_reason = ?2, revoked_at = ?3 WHERE id = ?1", params![id, reason, Self::now_rfc3339()])?;
        if changed == 0 {
            bail!("service identity client not found: {id}");
        }
        Ok(())
    }

    pub fn next_scim_token_id(&self, account_id: &str) -> Result<String> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) + 1 FROM scim_tokens WHERE account_id = ?1",
            params![account_id],
            |r| r.get(0),
        )?;
        Ok(format!("sct_{account_id}_{count}"))
    }

    pub fn upsert_scim_token(&self, token: &ScimTokenInput) -> Result<()> {
        self.conn.execute(
            "INSERT INTO scim_tokens (id, account_id, label, secret_hash, created_at, revoked_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL) ON CONFLICT(id) DO UPDATE SET label = excluded.label, secret_hash = excluded.secret_hash",
            params![token.id, token.account_id, token.label, token.secret_hash, token.created_at],
        )?;
        Ok(())
    }

    pub fn revoke_scim_token(
        &self,
        account_id: &str,
        token_id: &str,
        revoked_at: &str,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE scim_tokens SET revoked_at = ?3 WHERE account_id = ?1 AND id = ?2",
            params![account_id, token_id, revoked_at],
        )?;
        Ok(changed > 0)
    }

    pub fn list_scim_tokens(&self, account_id: &str) -> Result<Vec<ScimTokenSnapshot>> {
        let mut stmt = self.conn.prepare("SELECT id, account_id, label, created_at, revoked_at FROM scim_tokens WHERE account_id = ?1 ORDER BY id")?;
        Ok(stmt
            .query_map(params![account_id], |r| {
                Ok(ScimTokenSnapshot {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    label: r.get(2)?,
                    created_at: r.get(3)?,
                    revoked_at: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn all_scim_tokens(&self) -> Result<Vec<ScimTokenRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, account_id, label, created_at, revoked_at, secret_hash FROM scim_tokens ORDER BY id",
        )?;
        Ok(stmt
            .query_map([], |r| {
                Ok(ScimTokenRecord {
                    id: r.get(0)?,
                    account_id: r.get(1)?,
                    label: r.get(2)?,
                    created_at: r.get(3)?,
                    revoked_at: r.get(4)?,
                    secret_hash: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn next_scim_user_id(&self, account_id: &str) -> Result<String> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) + 1 FROM scim_users WHERE account_id = ?1",
            params![account_id],
            |r| r.get(0),
        )?;
        Ok(format!("scu_{account_id}_{count}"))
    }

    pub fn upsert_scim_user(&self, user: &ScimUserRecord) -> Result<()> {
        self.conn.execute("INSERT INTO scim_users (id, account_id, user_name, display_name, active, lifecycle, external_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(account_id, id) DO UPDATE SET user_name=excluded.user_name, display_name=excluded.display_name, active=excluded.active, lifecycle=excluded.lifecycle, external_id=excluded.external_id, updated_at=excluded.updated_at", params![user.id, user.account_id, user.user_name, user.display_name, user.active as i64, user.lifecycle, user.external_id, user.created_at, user.updated_at])?;
        Ok(())
    }

    fn scim_user_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScimUserRecord> {
        Ok(ScimUserRecord {
            id: row.get(0)?,
            account_id: row.get(1)?,
            user_name: row.get(2)?,
            display_name: row.get(3)?,
            active: row.get::<_, i64>(4)? != 0,
            lifecycle: row.get(5)?,
            external_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    pub fn get_scim_user(&self, account_id: &str, id: &str) -> Result<Option<ScimUserRecord>> {
        self.conn.query_row("SELECT id, account_id, user_name, display_name, active, lifecycle, external_id, created_at, updated_at FROM scim_users WHERE account_id = ?1 AND id = ?2", params![account_id, id], Self::scim_user_from_row).optional().map_err(Into::into)
    }

    pub fn get_scim_user_by_name(
        &self,
        account_id: &str,
        user_name: &str,
    ) -> Result<Option<ScimUserRecord>> {
        self.conn.query_row("SELECT id, account_id, user_name, display_name, active, lifecycle, external_id, created_at, updated_at FROM scim_users WHERE account_id = ?1 AND user_name = ?2", params![account_id, user_name], Self::scim_user_from_row).optional().map_err(Into::into)
    }

    pub fn list_scim_users(
        &self,
        account_id: &str,
        user_name_filter: Option<&str>,
    ) -> Result<Vec<ScimUserRecord>> {
        let mut stmt = self.conn.prepare(if user_name_filter.is_some() { "SELECT id, account_id, user_name, display_name, active, lifecycle, external_id, created_at, updated_at FROM scim_users WHERE account_id = ?1 AND user_name = ?2 ORDER BY user_name" } else { "SELECT id, account_id, user_name, display_name, active, lifecycle, external_id, created_at, updated_at FROM scim_users WHERE account_id = ?1 ORDER BY user_name" })?;
        let rows = match user_name_filter {
            Some(filter) => stmt
                .query_map(params![account_id, filter], Self::scim_user_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![account_id], Self::scim_user_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }

    pub fn next_scim_group_id(&self, account_id: &str) -> Result<String> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) + 1 FROM scim_groups WHERE account_id = ?1",
            params![account_id],
            |r| r.get(0),
        )?;
        Ok(format!("scg_{account_id}_{count}"))
    }

    pub fn upsert_scim_group(&self, group: &ScimGroupRecord) -> Result<()> {
        self.conn.execute("INSERT INTO scim_groups (id, account_id, display_name, role, members_json, tombstoned, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(account_id, id) DO UPDATE SET display_name=excluded.display_name, role=excluded.role, members_json=excluded.members_json, tombstoned=excluded.tombstoned, updated_at=excluded.updated_at", params![group.id, group.account_id, group.display_name, group.role, serde_json::to_string(&group.members)?, group.tombstoned as i64, group.created_at, group.updated_at])?;
        Ok(())
    }

    fn scim_group_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScimGroupRecord> {
        let members_json: String = row.get(4)?;
        Ok(ScimGroupRecord {
            id: row.get(0)?,
            account_id: row.get(1)?,
            display_name: row.get(2)?,
            role: row.get(3)?,
            members: serde_json::from_str(&members_json).unwrap_or_default(),
            tombstoned: row.get::<_, i64>(5)? != 0,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    pub fn get_scim_group(&self, account_id: &str, id: &str) -> Result<Option<ScimGroupRecord>> {
        self.conn.query_row("SELECT id, account_id, display_name, role, members_json, tombstoned, created_at, updated_at FROM scim_groups WHERE account_id = ?1 AND id = ?2", params![account_id, id], Self::scim_group_from_row).optional().map_err(Into::into)
    }

    pub fn list_scim_groups(&self, account_id: &str) -> Result<Vec<ScimGroupRecord>> {
        let mut stmt = self.conn.prepare("SELECT id, account_id, display_name, role, members_json, tombstoned, created_at, updated_at FROM scim_groups WHERE account_id = ?1 ORDER BY display_name")?;
        Ok(stmt
            .query_map(params![account_id], Self::scim_group_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn append_audit_event(
        &self,
        account_id: &str,
        organization_id: Option<&str>,
        event: &AuditEvent,
    ) -> Result<i64> {
        self.conn.execute("INSERT INTO audit_events (account_id, organization_id, event_json) VALUES (?1, ?2, ?3)", params![account_id, organization_id, serde_json::to_string(event)?])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn search_audit_events(
        &self,
        account_id: &str,
        organization_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<CatalogAuditEvent>> {
        let sql_account = "SELECT id, account_id, organization_id, event_json, created_at FROM audit_events WHERE account_id = ?1 ORDER BY id DESC LIMIT ?2";
        let sql_org = "SELECT id, account_id, organization_id, event_json, created_at FROM audit_events WHERE account_id = ?1 AND organization_id = ?2 ORDER BY id DESC LIMIT ?3";
        let mut stmt = self.conn.prepare(if organization_id.is_some() {
            sql_org
        } else {
            sql_account
        })?;
        let map_row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<CatalogAuditEvent> {
            let event_json: String = r.get(3)?;
            let event: Value = serde_json::from_str(&event_json).unwrap_or(Value::Null);
            let action = event
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            Ok(CatalogAuditEvent {
                id: r.get(0)?,
                account_id: r.get(1)?,
                organization_id: r.get(2)?,
                action,
                event,
                created_at: r.get(4)?,
            })
        };
        let rows = match organization_id {
            Some(org) => stmt
                .query_map(params![account_id, org, limit], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            None => stmt
                .query_map(params![account_id, limit], map_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        };
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn scalar_i64(path: &std::path::Path, sql: &str) -> i64 {
        let conn = Connection::open(path).unwrap();
        conn.query_row(sql, [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn test_catalog_open_and_migrate() {
        let catalog = Catalog::open(":memory:").unwrap();
        let tables = catalog.list_tables("opensnow", "public").unwrap();
        assert!(tables.is_empty());
    }

    #[test]
    fn test_catalog_file_init_and_migration_rerun_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/catalog.db");
        let path_str = path.to_str().unwrap();

        let catalog = Catalog::open(path_str).unwrap();
        catalog
            .register_table(
                "opensnow",
                "public",
                "sample_orders",
                "/warehouse/opensnow/public/sample_orders.parquet",
            )
            .unwrap();
        catalog
            .create_warehouse("analytics", "medium", 1, 4, 300)
            .unwrap();
        drop(catalog);

        let catalog = Catalog::open(path_str).unwrap();
        let tables = catalog.list_tables("opensnow", "public").unwrap();
        let warehouses = catalog.list_warehouses().unwrap();
        drop(catalog);

        assert!(path.exists());
        assert_eq!(
            scalar_i64(&path, "SELECT version FROM catalog_migrations"),
            Catalog::SCHEMA_VERSION
        );
        assert_eq!(
            scalar_i64(&path, "SELECT COUNT(*) FROM tenants WHERE id = 'default'"),
            1
        );
        assert_eq!(
            scalar_i64(
                &path,
                "SELECT COUNT(*) FROM databases WHERE name = 'opensnow'"
            ),
            1
        );
        assert_eq!(
            scalar_i64(&path, "SELECT COUNT(*) FROM schemas WHERE name = 'public'"),
            1
        );
        assert_eq!(
            scalar_i64(
                &path,
                "SELECT COUNT(*) FROM warehouses WHERE name = 'default'"
            ),
            1
        );
        assert!(tables.iter().any(|(name, _)| name == "sample_orders"));
        assert!(warehouses.iter().any(|w| w.name == "analytics"));
    }

    #[test]
    fn test_catalog_upgrade_backfills_legacy_tenant_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE query_history (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    submitted_at TEXT NOT NULL DEFAULT (datetime('now')),
                    user_name TEXT,
                    warehouse TEXT NOT NULL,
                    sql TEXT NOT NULL,
                    duration_ms INTEGER NOT NULL,
                    rows_returned INTEGER NOT NULL,
                    rows_scanned INTEGER,
                    status TEXT NOT NULL
                );
                INSERT INTO query_history (warehouse, sql, duration_ms, rows_returned, status)
                    VALUES ('default', 'SELECT 1', 1, 1, 'success');
                CREATE TABLE materialized_views (
                    name TEXT PRIMARY KEY,
                    sql TEXT NOT NULL,
                    last_refreshed TEXT NOT NULL,
                    parquet_path TEXT NOT NULL
                );
                INSERT INTO materialized_views (name, sql, last_refreshed, parquet_path)
                    VALUES ('mv_legacy', 'SELECT 1', '2026-01-01T00:00:00Z', '/warehouse/mv_legacy.parquet');
                ",
            )
            .unwrap();
        }

        let catalog = Catalog::open(path.to_str().unwrap()).unwrap();
        let queries = catalog
            .recent_queries_for_tenant(DEFAULT_TENANT, 10)
            .unwrap();
        let views = catalog
            .list_materialized_views_for_tenant(DEFAULT_TENANT)
            .unwrap();

        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].sql, "SELECT 1");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].name, "mv_legacy");
    }

    #[test]
    fn test_reset_runtime_state_preserves_catalog_tables_and_sample_locations() {
        let catalog = Catalog::open(":memory:").unwrap();
        catalog
            .register_table(
                "opensnow",
                "public",
                "sample_orders",
                "/warehouse/opensnow/public/sample_orders.parquet",
            )
            .unwrap();
        catalog
            .upsert_materialized_view(
                "mv_orders",
                "SELECT * FROM sample_orders",
                "/warehouse/mv_orders.parquet",
            )
            .unwrap();
        catalog
            .insert_query_record(&QueryRecordInput::for_default_tenant(
                "default", "SELECT 1", 1, 1, "success",
            ))
            .unwrap();

        catalog.reset_runtime_state().unwrap();

        let tables = catalog.list_tables("opensnow", "public").unwrap();
        assert_eq!(
            tables,
            vec![(
                "sample_orders".to_string(),
                "/warehouse/opensnow/public/sample_orders.parquet".to_string()
            )]
        );
        assert!(catalog.list_materialized_views().unwrap().is_empty());
        assert!(catalog.recent_queries(10).unwrap().is_empty());
        assert!(catalog.get_warehouse("default").unwrap().is_some());
        assert!(catalog.get_tenant(DEFAULT_TENANT).unwrap().is_some());
    }

    #[test]
    fn test_register_and_list() {
        let catalog = Catalog::open(":memory:").unwrap();
        catalog
            .register_table("opensnow", "public", "orders", "/data/orders.parquet")
            .unwrap();
        let tables = catalog.list_tables("opensnow", "public").unwrap();
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].0, "orders");
    }

    #[test]
    fn test_default_warehouse_exists() {
        let catalog = Catalog::open(":memory:").unwrap();
        let warehouses = catalog.list_warehouses().unwrap();
        assert!(!warehouses.is_empty());
        assert!(warehouses.iter().any(|w| w.name == "default"));

        let wh = catalog.get_warehouse("default").unwrap().unwrap();
        assert_eq!(wh.size, "small");
        assert_eq!(wh.state, "SUSPENDED");
    }

    #[test]
    fn test_create_get_update_warehouse() {
        let catalog = Catalog::open(":memory:").unwrap();

        // Create
        catalog
            .create_warehouse("analytics", "medium", 1, 8, 600)
            .unwrap();

        // Get
        let wh = catalog.get_warehouse("analytics").unwrap().unwrap();
        assert_eq!(wh.name, "analytics");
        assert_eq!(wh.size, "medium");
        assert_eq!(wh.state, "SUSPENDED");
        assert_eq!(wh.min_nodes, 1);
        assert_eq!(wh.max_nodes, 8);
        assert_eq!(wh.auto_suspend_seconds, 600);

        // Update state
        catalog
            .update_warehouse_state("analytics", "RUNNING")
            .unwrap();
        let wh = catalog.get_warehouse("analytics").unwrap().unwrap();
        assert_eq!(wh.state, "RUNNING");

        // List should have default + analytics
        let all = catalog.list_warehouses().unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_update_nonexistent_warehouse() {
        let catalog = Catalog::open(":memory:").unwrap();
        let result = catalog.update_warehouse_state("nope", "RUNNING");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_nonexistent_warehouse() {
        let catalog = Catalog::open(":memory:").unwrap();
        let wh = catalog.get_warehouse("nope").unwrap();
        assert!(wh.is_none());
    }

    #[test]
    fn test_materialized_view_crud() {
        let catalog = Catalog::open(":memory:").unwrap();

        // Initially empty
        assert!(catalog.list_materialized_views().unwrap().is_empty());
        assert!(
            catalog
                .get_materialized_view("mv_orders")
                .unwrap()
                .is_none()
        );

        // Insert
        catalog
            .upsert_materialized_view("mv_orders", "SELECT 1", "/tmp/mv_orders.parquet")
            .unwrap();
        let mv = catalog.get_materialized_view("mv_orders").unwrap().unwrap();
        assert_eq!(mv.name, "mv_orders");
        assert_eq!(mv.sql, "SELECT 1");
        assert_eq!(mv.parquet_path, "/tmp/mv_orders.parquet");
        assert!(!mv.last_refreshed.is_empty());

        // Update via upsert
        catalog
            .upsert_materialized_view("mv_orders", "SELECT 2", "/tmp/mv_orders.parquet")
            .unwrap();
        let mv = catalog.get_materialized_view("mv_orders").unwrap().unwrap();
        assert_eq!(mv.sql, "SELECT 2");

        // Touch updates timestamp without changing sql
        let before = mv.last_refreshed.clone();
        std::thread::sleep(std::time::Duration::from_millis(5));
        catalog.touch_materialized_view("mv_orders").unwrap();
        let after = catalog.get_materialized_view("mv_orders").unwrap().unwrap();
        assert_ne!(before, after.last_refreshed);
        assert_eq!(after.sql, "SELECT 2");

        // List
        catalog
            .upsert_materialized_view("mv_users", "SELECT 3", "/tmp/mv_users.parquet")
            .unwrap();
        let all = catalog.list_materialized_views().unwrap();
        assert_eq!(all.len(), 2);

        // Delete
        assert!(catalog.delete_materialized_view("mv_orders").unwrap());
        assert!(!catalog.delete_materialized_view("mv_orders").unwrap());
        assert!(
            catalog
                .get_materialized_view("mv_orders")
                .unwrap()
                .is_none()
        );
        assert_eq!(catalog.list_materialized_views().unwrap().len(), 1);
    }

    #[test]
    fn test_default_tenant_seeded() {
        let catalog = Catalog::open(":memory:").unwrap();
        let tenants = catalog.list_tenants().unwrap();
        assert_eq!(tenants.len(), 1);
        assert_eq!(tenants[0].id, "default");
    }

    #[test]
    fn test_create_and_get_tenant() {
        let catalog = Catalog::open(":memory:").unwrap();
        let t = catalog.create_tenant("Acme Corp").unwrap();
        assert_eq!(t.id, "acme-corp");
        assert_eq!(t.name, "Acme Corp");
        assert!(catalog.get_tenant("acme-corp").unwrap().is_some());
        assert!(catalog.get_tenant("missing").unwrap().is_none());
        assert_eq!(catalog.list_tenants().unwrap().len(), 2);
    }

    #[test]
    fn test_query_history_scoped_by_tenant() {
        let catalog = Catalog::open(":memory:").unwrap();
        catalog.create_tenant("blue").unwrap();
        catalog.create_tenant("red").unwrap();

        let blue = QueryRecordInput {
            user_name: None,
            warehouse: "default".into(),
            sql: "SELECT 1".into(),
            duration_ms: 1,
            rows_returned: 1,
            rows_scanned: None,
            status: "success".into(),
        };
        let red = QueryRecordInput {
            user_name: None,
            warehouse: "default".into(),
            sql: "SELECT 2".into(),
            duration_ms: 2,
            rows_returned: 2,
            rows_scanned: None,
            status: "success".into(),
        };
        catalog
            .insert_query_record_for_tenant("blue", &blue)
            .unwrap();
        catalog.insert_query_record_for_tenant("red", &red).unwrap();

        let blue_recent = catalog.recent_queries_for_tenant("blue", 10).unwrap();
        assert_eq!(blue_recent.len(), 1);
        assert_eq!(blue_recent[0].sql, "SELECT 1");

        let red_recent = catalog.recent_queries_for_tenant("red", 10).unwrap();
        assert_eq!(red_recent.len(), 1);
        assert_eq!(red_recent[0].sql, "SELECT 2");

        // The unscoped recent_queries returns rows from every tenant.
        assert_eq!(catalog.recent_queries(10).unwrap().len(), 2);
    }

    #[test]
    fn test_mv_scoped_by_tenant() {
        let catalog = Catalog::open(":memory:").unwrap();
        catalog.create_tenant("blue").unwrap();
        catalog.create_tenant("red").unwrap();

        catalog
            .upsert_materialized_view_for_tenant("blue", "mv_orders", "SELECT 1", "/b.parquet")
            .unwrap();
        catalog
            .upsert_materialized_view_for_tenant("red", "mv_orders", "SELECT 2", "/r.parquet")
            .unwrap();

        // Each tenant only sees their MVs.
        let blue = catalog.list_materialized_views_for_tenant("blue").unwrap();
        let red = catalog.list_materialized_views_for_tenant("red").unwrap();
        assert_eq!(blue.len(), 1);
        assert_eq!(red.len(), 1);
        assert_eq!(blue[0].sql, "SELECT 1");
        assert_eq!(red[0].sql, "SELECT 2");

        // Default tenant sees nothing.
        assert!(catalog.list_materialized_views().unwrap().is_empty());

        // Deleting in one tenant doesn't touch the other.
        assert!(
            catalog
                .delete_materialized_view_for_tenant("blue", "mv_orders")
                .unwrap()
        );
        assert!(
            catalog
                .list_materialized_views_for_tenant("blue")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            catalog
                .list_materialized_views_for_tenant("red")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn test_scim_and_service_identity_catalog_apis_are_account_scoped_and_do_not_leak_raw_tokens() {
        let catalog = Catalog::open(":memory:").unwrap();
        let created = "2026-05-30T00:00:00Z".to_string();

        let svc = catalog
            .create_service_identity_client(ServiceIdentityClientInput {
                id: "svc_acme_prod".to_string(),
                account_id: "acct_acme".to_string(),
                workspace_id: Some("ws_acme_default".to_string()),
                secret_hash: "$argon2id$v=19$service-secret-hash".to_string(),
                role: "ACCOUNTADMIN".to_string(),
                scopes: vec!["sql:execute".to_string(), "tables:read".to_string()],
                expires_at: Some("2026-06-30T00:00:00Z".to_string()),
            })
            .unwrap();
        assert_eq!(svc.status, "active");
        assert_eq!(
            catalog
                .list_service_identity_clients(Some("acct_other"))
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            catalog
                .list_service_identity_clients(Some("acct_acme"))
                .unwrap()
                .len(),
            1
        );
        catalog.mark_service_identity_used("svc_acme_prod").unwrap();
        assert!(
            catalog
                .get_service_identity_client("svc_acme_prod")
                .unwrap()
                .unwrap()
                .last_used_at
                .is_some()
        );
        catalog
            .rotate_service_identity_secret(
                "svc_acme_prod",
                "$argon2id$v=19$service-secret-hash-rotated",
            )
            .unwrap();
        let rotated = catalog
            .get_service_identity_client("svc_acme_prod")
            .unwrap()
            .unwrap();
        assert_eq!(
            rotated.secret_hash,
            "$argon2id$v=19$service-secret-hash-rotated"
        );
        assert!(rotated.rotated_at.is_some());
        catalog
            .revoke_service_identity_client("svc_acme_prod", "operator-request")
            .unwrap();
        let revoked = catalog
            .get_service_identity_client("svc_acme_prod")
            .unwrap()
            .unwrap();
        assert_eq!(revoked.status, "revoked");
        assert_eq!(revoked.status_reason.as_deref(), Some("operator-request"));

        catalog
            .upsert_scim_token(&ScimTokenInput {
                id: "sct_acme_1".to_string(),
                account_id: "acct_acme".to_string(),
                label: "Azure AD".to_string(),
                secret_hash: "$argon2id$v=19$scim-token-hash".to_string(),
                created_at: created.clone(),
            })
            .unwrap();
        let visible_tokens = catalog.list_scim_tokens("acct_acme").unwrap();
        assert_eq!(visible_tokens.len(), 1);
        assert_eq!(visible_tokens[0].label, "Azure AD");
        assert_eq!(catalog.list_scim_tokens("acct_other").unwrap().len(), 0);
        let serialized = serde_json::to_string(&visible_tokens).unwrap();
        assert!(!serialized.contains("scim-token-hash"));
        assert!(!serialized.contains("secret_hash"));
        assert!(
            catalog
                .revoke_scim_token("acct_acme", "sct_acme_1", "2026-05-30T01:00:00Z")
                .unwrap()
        );
        assert!(
            catalog
                .list_scim_tokens("acct_acme")
                .unwrap()
                .first()
                .unwrap()
                .revoked_at
                .is_some()
        );

        catalog
            .upsert_scim_user(&ScimUserRecord {
                id: "usr_acme_1".to_string(),
                account_id: "acct_acme".to_string(),
                user_name: "alice@example.com".to_string(),
                display_name: Some("Alice Example".to_string()),
                active: true,
                lifecycle: "active".to_string(),
                external_id: Some("aad-1".to_string()),
                created_at: created.clone(),
                updated_at: created.clone(),
            })
            .unwrap();
        assert!(
            catalog
                .get_scim_user("acct_other", "usr_acme_1")
                .unwrap()
                .is_none()
        );
        assert!(
            catalog
                .get_scim_user_by_name("acct_acme", "alice@example.com")
                .unwrap()
                .is_some()
        );
        assert_eq!(catalog.list_scim_users("acct_acme", None).unwrap().len(), 1);

        catalog
            .upsert_scim_group(&ScimGroupRecord {
                id: "grp_acme_admins".to_string(),
                account_id: "acct_acme".to_string(),
                display_name: "Admins".to_string(),
                role: "ACCOUNTADMIN".to_string(),
                members: vec!["usr_acme_1".to_string()],
                tombstoned: false,
                created_at: created.clone(),
                updated_at: created,
            })
            .unwrap();
        assert_eq!(
            catalog
                .get_scim_group("acct_acme", "grp_acme_admins")
                .unwrap()
                .unwrap()
                .members,
            vec!["usr_acme_1".to_string()]
        );
        assert_eq!(catalog.list_scim_groups("acct_other").unwrap().len(), 0);
    }

    fn aws_entitlement(
        org_id: &str,
        state: opensnow_auth::EntitlementState,
        features: &[&str],
    ) -> opensnow_auth::EntitlementCheck {
        features.iter().fold(
            opensnow_auth::EntitlementCheck::new(
                opensnow_auth::MarketplaceIdentity::aws(
                    org_id,
                    "aws-customer-acme",
                    "opensnow-enterprise",
                    "entitlement-acme",
                ),
                opensnow_auth::EntitlementPlan::Enterprise,
                state,
            ),
            |check, feature| check.with_feature(*feature),
        )
    }

    #[test]
    fn enterprise_account_registration_requires_active_matching_entitlement_and_audits_decision() {
        let catalog = Catalog::open(":memory:").unwrap();
        let denied = catalog.register_enterprise_account("Acme Corp", "owner@acme.test", None);
        assert!(denied.is_err());

        let inactive = catalog.register_enterprise_account(
            "Acme Corp",
            "owner@acme.test",
            Some(&aws_entitlement(
                "org_acme-corp",
                opensnow_auth::EntitlementState::Suspended,
                &["account.activate"],
            )),
        );
        assert!(inactive.is_err());

        let wrong_org = catalog.register_enterprise_account(
            "Acme Corp",
            "owner@acme.test",
            Some(&aws_entitlement(
                "org_other",
                opensnow_auth::EntitlementState::Active,
                &["account.activate"],
            )),
        );
        assert!(wrong_org.is_err());

        let allowed = catalog
            .register_enterprise_account(
                "Acme Corp",
                "owner@acme.test",
                Some(&aws_entitlement(
                    "org_acme-corp",
                    opensnow_auth::EntitlementState::Active,
                    &["account.activate"],
                )),
            )
            .unwrap();
        assert_eq!(allowed.account.id, "acme-corp");
        assert_eq!(allowed.organization.id, "org_acme-corp");

        let audit = catalog
            .search_audit_events("acme-corp", Some("org_acme-corp"), 10)
            .unwrap();
        assert!(audit.iter().any(|event| {
            event.event["action"] == "account.activate"
                && event.event["result"] == "Allowed"
                && event.event["metadata_redacted"]["entitlement_id"] == "entitlement-acme"
        }));
        assert!(audit.iter().any(|event| {
            event.event["action"] == "account.activate"
                && event.event["result"] == "Denied"
                && event.event["metadata_redacted"]["reason"] == "missing_entitlement"
        }));
    }

    #[test]
    fn enterprise_warehouse_creation_requires_active_matching_entitlement_and_preserves_account_isolation()
     {
        let catalog = Catalog::open(":memory:").unwrap();
        let entitlement = aws_entitlement(
            "org_acme-corp",
            opensnow_auth::EntitlementState::Active,
            &["account.activate", "warehouse.activate"],
        )
        .with_warehouse_limit(1);
        catalog
            .register_enterprise_account("Acme Corp", "owner@acme.test", Some(&entitlement))
            .unwrap();

        let suspended = aws_entitlement(
            "org_acme-corp",
            opensnow_auth::EntitlementState::Suspended,
            &["warehouse.activate"],
        );
        assert!(
            catalog
                .create_enterprise_warehouse(
                    EnterpriseWarehouseRequest {
                        account_id: "acme-corp",
                        name: "analytics_suspended",
                        size: "small",
                        min_nodes: 1,
                        max_nodes: 1,
                        auto_suspend_seconds: 60,
                    },
                    Some(&suspended),
                )
                .is_err()
        );

        let wrong_org = aws_entitlement(
            "org_other",
            opensnow_auth::EntitlementState::Active,
            &["warehouse.activate"],
        );
        assert!(
            catalog
                .create_enterprise_warehouse(
                    EnterpriseWarehouseRequest {
                        account_id: "acme-corp",
                        name: "analytics_wrong",
                        size: "small",
                        min_nodes: 1,
                        max_nodes: 1,
                        auto_suspend_seconds: 60,
                    },
                    Some(&wrong_org),
                )
                .is_err()
        );

        catalog
            .create_enterprise_warehouse(
                EnterpriseWarehouseRequest {
                    account_id: "acme-corp",
                    name: "analytics",
                    size: "small",
                    min_nodes: 1,
                    max_nodes: 1,
                    auto_suspend_seconds: 60,
                },
                Some(&entitlement),
            )
            .unwrap();

        let other_entitlement = aws_entitlement(
            "org_beta-corp",
            opensnow_auth::EntitlementState::Active,
            &["account.activate", "warehouse.activate"],
        )
        .with_warehouse_limit(1);
        catalog
            .register_enterprise_account("Beta Corp", "owner@beta.test", Some(&other_entitlement))
            .unwrap();
        catalog
            .create_enterprise_warehouse(
                EnterpriseWarehouseRequest {
                    account_id: "beta-corp",
                    name: "beta_analytics",
                    size: "small",
                    min_nodes: 1,
                    max_nodes: 1,
                    auto_suspend_seconds: 60,
                },
                Some(&other_entitlement),
            )
            .unwrap();
        assert!(
            catalog
                .create_enterprise_warehouse(
                    EnterpriseWarehouseRequest {
                        account_id: "acme-corp",
                        name: "analytics_over_limit",
                        size: "small",
                        min_nodes: 1,
                        max_nodes: 1,
                        auto_suspend_seconds: 60,
                    },
                    Some(&entitlement),
                )
                .is_err()
        );
        assert!(
            catalog
                .create_enterprise_warehouse(
                    EnterpriseWarehouseRequest {
                        account_id: "other-account",
                        name: "escape",
                        size: "small",
                        min_nodes: 1,
                        max_nodes: 1,
                        auto_suspend_seconds: 60,
                    },
                    Some(&entitlement),
                )
                .is_err()
        );

        let audit = catalog
            .search_audit_events("acme-corp", Some("org_acme-corp"), 10)
            .unwrap();
        assert!(audit.iter().any(|event| {
            event.event["action"] == "warehouse.activate"
                && event.event["result"] == "Allowed"
                && event.event["resource_id"] == "analytics"
        }));
        assert!(audit.iter().any(|event| {
            event.event["action"] == "warehouse.activate"
                && event.event["result"] == "Denied"
                && event.event["metadata_redacted"]["reason"] == "inactive_entitlement"
        }));
    }

    #[test]
    fn audit_events_append_only_scopes_search_results_by_account_and_org() {
        let catalog = Catalog::open(":memory:").unwrap();
        let first = AuditEvent {
            event_time: chrono::DateTime::parse_from_rfc3339("2026-05-30T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            organization_id: "org_acme".to_string(),
            tenant_id: Some("acct_acme".to_string()),
            actor_type: "user".to_string(),
            actor_id: "alice".to_string(),
            actor_display: Some("Alice Example".to_string()),
            actor_auth_method: Some("scim-bearer".to_string()),
            action: "scim.user.create".to_string(),
            resource_type: "scim_user".to_string(),
            resource_id: "usr_acme_1".to_string(),
            resource_name: Some("alice@example.com".to_string()),
            result: opensnow_auth::AuditResult::Succeeded,
            trace_id: Some("req-1".to_string()),
            secret_handle_refs: Vec::new(),
            metadata_redacted: serde_json::Map::new(),
        };
        let second = AuditEvent {
            action: "scim.group.create".to_string(),
            resource_type: "scim_group".to_string(),
            resource_id: "grp_acme_admins".to_string(),
            resource_name: Some("Admins".to_string()),
            trace_id: Some("req-2".to_string()),
            ..first.clone()
        };
        let other_account = AuditEvent {
            organization_id: "org_other".to_string(),
            tenant_id: Some("acct_other".to_string()),
            trace_id: Some("req-3".to_string()),
            resource_id: "usr_other_1".to_string(),
            ..first.clone()
        };

        let first_id = catalog
            .append_audit_event("acct_acme", Some("org_acme"), &first)
            .unwrap();
        let second_id = catalog
            .append_audit_event("acct_acme", Some("org_acme"), &second)
            .unwrap();
        let other_id = catalog
            .append_audit_event("acct_other", Some("org_other"), &other_account)
            .unwrap();

        assert!(first_id < second_id);
        assert!(second_id < other_id);
        let acme_events = catalog
            .search_audit_events("acct_acme", Some("org_acme"), 10)
            .unwrap();
        assert_eq!(acme_events.len(), 2);
        assert_eq!(acme_events[0].id, second_id);
        assert_eq!(acme_events[1].id, first_id);
        assert!(acme_events.iter().all(|event| {
            event.account_id == "acct_acme" && event.organization_id.as_deref() == Some("org_acme")
        }));
        assert_eq!(acme_events[0].action, "scim.group.create");
        assert_eq!(acme_events[1].event["resource_id"], "usr_acme_1");
        assert!(
            catalog
                .search_audit_events("acct_other", Some("org_acme"), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_slugify_tenant_id() {
        assert_eq!(slugify_tenant_id("Acme Corp"), "acme-corp");
        assert_eq!(slugify_tenant_id("  Hello, World!  "), "hello-world");
        assert_eq!(
            slugify_tenant_id("multi___underscores"),
            "multi___underscores"
        );
        assert_eq!(slugify_tenant_id("Q1-2026"), "q1-2026");
    }

    #[test]
    fn test_insert_and_recent_queries() {
        let catalog = Catalog::open(":memory:").unwrap();

        let record1 = QueryRecordInput {
            user_name: Some("alice".to_string()),
            warehouse: "default".to_string(),
            sql: "SELECT 1".to_string(),
            duration_ms: 10,
            rows_returned: 1,
            rows_scanned: Some(1),
            status: "success".to_string(),
        };
        catalog.insert_query_record(&record1).unwrap();

        let record2 = QueryRecordInput {
            user_name: None,
            warehouse: "default".to_string(),
            sql: "SELECT 2".to_string(),
            duration_ms: 20,
            rows_returned: 1,
            rows_scanned: None,
            status: "error".to_string(),
        };
        catalog.insert_query_record(&record2).unwrap();

        let recent = catalog.recent_queries(10).unwrap();
        assert_eq!(recent.len(), 2);

        // Newest record should come first
        assert_eq!(recent[0].sql, "SELECT 2");
        assert_eq!(recent[0].status, "error");
        assert_eq!(recent[1].sql, "SELECT 1");
        assert_eq!(recent[1].status, "success");
    }
}
