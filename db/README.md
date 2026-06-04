# OpenSnow — Database Reference

All SQL schemas, migrations, and seed data in one place.
Run these in order to recreate the full database from scratch.

---

## Connection

| Deployment | Backend | Connection |
|---|---|---|
| Local / dev | SQLite (WAL mode) | `~/.opensnow/catalog.db` |
| Production | PostgreSQL | `$DATABASE_URL` |

---

## Migration Order

```
000_core.sql          — base tables (users, roles, tenants, warehouses)
001_sso.sql           — SSO/OIDC support (sso_role_mappings, tenant OIDC fields)
```

---

## 000_core.sql — Base Schema

> **SQLite**: created inline by the Rust crates at startup (see `opensnow-auth/src/users.rs`, `roles.rs`, `privileges.rs`).  
> **PostgreSQL**: run this file manually or via migration tooling.

```sql
-- users
CREATE TABLE IF NOT EXISTS users (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,  -- UUID in PG: uuid DEFAULT gen_random_uuid()
    username TEXT    NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    role     TEXT    NOT NULL DEFAULT 'PUBLIC',
    auth_provider TEXT NOT NULL DEFAULT 'native',  -- 'native' | 'oidc'
    created_at TEXT  NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- roles (Snowflake-style hierarchy)
CREATE TABLE IF NOT EXISTS roles (
    name        TEXT PRIMARY KEY,  -- e.g. ACCOUNTADMIN, SYSADMIN, PUBLIC
    description TEXT
);

INSERT OR IGNORE INTO roles (name) VALUES ('ACCOUNTADMIN'), ('SYSADMIN'), ('PUBLIC');

-- privileges (object-level access control)
CREATE TABLE IF NOT EXISTS privileges (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    grantee     TEXT NOT NULL,   -- role name
    object_type TEXT NOT NULL,   -- DATABASE, SCHEMA, TABLE, WAREHOUSE
    object_name TEXT NOT NULL,
    privilege   TEXT NOT NULL,   -- SELECT, INSERT, CREATE, USAGE, etc.
    UNIQUE(grantee, object_type, object_name, privilege)
);

-- tenants (for multi-tenant / SSO)
CREATE TABLE IF NOT EXISTS tenants (
    id   TEXT PRIMARY KEY,       -- UUID
    slug TEXT UNIQUE NOT NULL,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- warehouses (virtual compute pools; local/single-node state gates named query routing, Kubernetes worker isolation is roadmap)
-- CREATE WAREHOUSE validates safe unquoted names, known sizes, non-negative min/max/auto-suspend metadata, and max_nodes >= min_nodes before inserting rows.
-- min_nodes, max_nodes, and auto_suspend_seconds are stored/displayed metadata in the current local slice; they do not provision workers or enforce per-warehouse scaling/admission yet.
CREATE TABLE IF NOT EXISTS warehouses (
    id          TEXT PRIMARY KEY,
    name        TEXT UNIQUE NOT NULL,
    size        TEXT NOT NULL DEFAULT 'small',   -- xsmall, small, medium, large, xlarge; used for estimated credits
    state       TEXT NOT NULL DEFAULT 'SUSPENDED', -- named warehouses must be RESUMED before routed data queries
    min_nodes   INTEGER NOT NULL DEFAULT 0,
    max_nodes   INTEGER NOT NULL DEFAULT 4,
    auto_suspend_seconds INTEGER NOT NULL DEFAULT 300,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);
```

Runtime query history records the routed warehouse name and status for audit/cost reports; Prometheus adds per-warehouse query and estimated-credit counters. These are usage estimates, not a full cloud billing engine.

---

## 001_sso.sql — SSO / OIDC Support

> Source: `crates/opensnow-auth/migrations/001_sso.sql`  
> SQLite equivalent: `crates/opensnow-auth/src/sso.rs` → `apply_sso_schema()`

```sql
-- PostgreSQL version (production)
BEGIN;

-- Extend tenants with OIDC fields
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS sso_enabled       BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_issuer       TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_client_id    TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_client_secret TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_scopes       TEXT DEFAULT 'openid email profile';
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS allowed_domains   JSONB DEFAULT '[]';
-- Example allowed_domains: '["acme.com", "acme.io"]'

-- SSO role mappings: IdP group/claim → OpenSnow role
-- When a user logs in via OIDC, their IdP claims are matched
-- against these rows to auto-assign roles.
CREATE TABLE IF NOT EXISTS sso_role_mappings (
    id             UUID    PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      UUID    NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    idp_claim_key  TEXT    NOT NULL DEFAULT 'groups',  -- JWT claim to inspect: 'groups', 'department', etc.
    idp_claim_value TEXT   NOT NULL,                   -- Value to match: 'engineering', 'finance', etc.
    role_id        TEXT    NOT NULL REFERENCES roles(name) ON DELETE CASCADE,
    created_at     TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(tenant_id, idp_claim_key, idp_claim_value, role_id)
);

CREATE INDEX IF NOT EXISTS idx_sso_role_mappings_tenant ON sso_role_mappings(tenant_id);

COMMIT;
```

**How SSO role mapping works:**

```
User logs in via Google/Okta/Azure
  → OIDC token contains: { "groups": ["engineering", "admins"] }
  → Lookup sso_role_mappings for tenant
  → Match: idp_claim_key="groups", idp_claim_value="engineering" → role=SYSADMIN
  → Match: idp_claim_key="groups", idp_claim_value="admins"      → role=ACCOUNTADMIN
  → User auto-assigned both roles on login
```

---

## Seed Data

### Default admin user (auto-created on first start)

```sql
-- Created automatically by UserStore::new() if no users exist
-- Password set via OPENSNOW_ADMIN_PASSWORD env var (default: 'admin')
INSERT INTO users (username, password_hash, role)
VALUES ('admin', '<argon2id hash>', 'ACCOUNTADMIN');
```

### Default roles

```sql
INSERT OR IGNORE INTO roles (name) VALUES
  ('ACCOUNTADMIN'),
  ('SYSADMIN'),
  ('PUBLIC');
```

---

## Recreate from scratch

### Local (SQLite)

```bash
# Tables are auto-created by the Rust binary on first start.
opensnow start
# Then apply SSO schema manually if needed:
opensnow shell --exec "SELECT * FROM users;"  # verify
```

### Production (PostgreSQL)

```bash
export DATABASE_URL=postgresql://user:pass@host:5432/opensnow

# Apply in order:
psql $DATABASE_URL -f db/migrations/000_core.sql
psql $DATABASE_URL -f db/migrations/001_sso.sql
```

---

## File locations

| File | Purpose |
|---|---|
| `db/README.md` | This file — canonical schema reference |
| `db/migrations/000_core.sql` | Base tables (PostgreSQL) |
| `crates/opensnow-auth/migrations/001_sso.sql` | SSO migration (PostgreSQL) |
| `crates/opensnow-auth/src/users.rs` | Users table (SQLite, inline Rust) |
| `crates/opensnow-auth/src/roles.rs` | Roles table (SQLite, inline Rust) |
| `crates/opensnow-auth/src/privileges.rs` | Privileges table (SQLite, inline Rust) |
| `crates/opensnow-auth/src/sso.rs` | SSO schema + `apply_sso_schema()` (SQLite) |
