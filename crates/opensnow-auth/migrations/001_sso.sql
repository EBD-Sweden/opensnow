-- 001_sso.sql — SSO/OIDC support for OpenSnow
BEGIN;

ALTER TABLE tenants ADD COLUMN IF NOT EXISTS sso_enabled BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_issuer TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_client_id TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_client_secret TEXT;
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS oidc_scopes TEXT DEFAULT 'openid email profile';
ALTER TABLE tenants ADD COLUMN IF NOT EXISTS allowed_domains JSONB DEFAULT '[]';

CREATE TABLE IF NOT EXISTS sso_role_mappings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    idp_claim_key TEXT NOT NULL DEFAULT 'groups',
    idp_claim_value TEXT NOT NULL,
    role_id UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(tenant_id, idp_claim_key, idp_claim_value, role_id)
);

CREATE INDEX IF NOT EXISTS idx_sso_role_mappings_tenant ON sso_role_mappings(tenant_id);

CREATE TABLE IF NOT EXISTS sso_idp_connections (
    account_id TEXT NOT NULL,
    id TEXT NOT NULL,
    protocol TEXT NOT NULL CHECK (protocol IN ('oidc', 'saml')),
    enabled BOOLEAN NOT NULL DEFAULT true,
    issuer TEXT NOT NULL,
    client_id TEXT NOT NULL,
    client_secret_handle TEXT,
    allowed_domains JSONB NOT NULL DEFAULT '[]',
    scopes TEXT NOT NULL DEFAULT 'openid email profile',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    PRIMARY KEY (account_id, id)
);

CREATE INDEX IF NOT EXISTS idx_sso_idp_connections_account ON sso_idp_connections(account_id);

CREATE TABLE IF NOT EXISTS sso_idp_role_mappings (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    idp_claim_key TEXT NOT NULL DEFAULT 'groups',
    idp_claim_value TEXT NOT NULL,
    role_id TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(account_id, connection_id, idp_claim_key, idp_claim_value, role_id),
    FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sso_oidc_login_transactions (
    state TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    nonce TEXT NOT NULL,
    pkce_verifier TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    consumed_at TIMESTAMPTZ,
    FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sso_sessions (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    connection_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    email TEXT NOT NULL,
    roles JSONB NOT NULL DEFAULT '[]',
    issued_at BIGINT NOT NULL,
    expires_at BIGINT NOT NULL,
    revoked_at BIGINT,
    FOREIGN KEY (account_id, connection_id) REFERENCES sso_idp_connections(account_id, id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_sso_sessions_account ON sso_sessions(account_id);
CREATE INDEX IF NOT EXISTS idx_sso_sessions_subject ON sso_sessions(account_id, subject);

COMMIT;
