-- 000_core.sql — OpenSnow base schema (PostgreSQL)
-- Run first, before any other migrations.
-- SQLite equivalent: auto-created by Rust crates at startup.

BEGIN;

-- Users
CREATE TABLE IF NOT EXISTS users (
    id            UUID    PRIMARY KEY DEFAULT gen_random_uuid(),
    username      TEXT    NOT NULL UNIQUE,
    password_hash TEXT    NOT NULL,
    role          TEXT    NOT NULL DEFAULT 'PUBLIC',
    auth_provider TEXT    NOT NULL DEFAULT 'native',  -- 'native' | 'oidc'
    created_at    TIMESTAMPTZ DEFAULT NOW()
);

-- Roles (Snowflake-style hierarchy)
CREATE TABLE IF NOT EXISTS roles (
    name        TEXT PRIMARY KEY,
    description TEXT
);

INSERT INTO roles (name) VALUES ('ACCOUNTADMIN'), ('SYSADMIN'), ('PUBLIC')
ON CONFLICT DO NOTHING;

-- Privileges (object-level RBAC)
CREATE TABLE IF NOT EXISTS privileges (
    id          UUID    PRIMARY KEY DEFAULT gen_random_uuid(),
    grantee     TEXT    NOT NULL,        -- role name
    object_type TEXT    NOT NULL,        -- DATABASE, SCHEMA, TABLE, WAREHOUSE
    object_name TEXT    NOT NULL,
    privilege   TEXT    NOT NULL,        -- SELECT, INSERT, CREATE, USAGE, etc.
    granted_at  TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(grantee, object_type, object_name, privilege)
);

-- Tenants (multi-tenant / SSO)
CREATE TABLE IF NOT EXISTS tenants (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    slug       TEXT UNIQUE NOT NULL,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Warehouses (virtual compute pools)
CREATE TABLE IF NOT EXISTS warehouses (
    id                     UUID    PRIMARY KEY DEFAULT gen_random_uuid(),
    name                   TEXT    UNIQUE NOT NULL,
    size                   TEXT    NOT NULL DEFAULT 'small',
    state                  TEXT    NOT NULL DEFAULT 'SUSPENDED',
    min_nodes              INTEGER NOT NULL DEFAULT 0,
    max_nodes              INTEGER NOT NULL DEFAULT 4,
    auto_suspend_seconds   INTEGER NOT NULL DEFAULT 300,
    created_at             TIMESTAMPTZ DEFAULT NOW()
);

COMMIT;
