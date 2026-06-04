# OpenSnow CLI

OpenSnow CLI is the command-line interface for OpenSnow. The supported lane name is `opensnow-cli`.

## Contract and readiness commands

The CLI exposes a stable agent-facing contract so automation can discover supported commands and output schemas without scraping help text.

```bash
opensnow cli contract
opensnow cli contract --format json
opensnow cli doctor
opensnow cli doctor --format json
```

`contract` and `doctor` currently emit the same `OpenSnowCliReport` schema. The report contains:

- `product`: always `opensnow`
- `lane`: always `opensnow-cli`
- `target`: `enterprise-self-service-account-infra`
- `commands`: command names, required inputs, optional inputs, and output shape
- `config`: default config file, global flags, environment variables, and secret policy
- `schemas`: stable schema names for automation
- `agent_contract`: machine-facing command/API/output-format contract
- `checks`: local config readiness checks for storage, catalog, enterprise secrets, entitlements, and pgwire exposure

The JSON output intentionally does not include raw secret values.

## Core commands

```bash
# Initialize local config and optional sample warehouse data
opensnow init --with-sample-data --industry=telecom|banking|both

# Start the server; pgwire requires explicit trusted-local opt-in
opensnow start --enable-pgwire

# Run a one-shot SQL statement without a server
opensnow local 'SELECT 1'

# Run the interactive shell or a single shell command
opensnow shell
opensnow shell -c 'SELECT 1'

# Inspect recent query history and reset ephemeral runtime state
opensnow queries --limit 20
opensnow reset-runtime-state

# Enterprise self-service account bootstrap
opensnow account-register --account-name 'Acme Corp' --owner-email owner@example.com
opensnow account-workspace-create --account-id acme-corp --name analytics
```

## Configuration

Global CLI config flag:

```bash
opensnow --config /path/to/opensnow.toml cli doctor --format json
```

Default config file created by `opensnow init`: `opensnow.toml`.

Relevant environment variables surfaced in the CLI contract:

- `OPENSNOW_STORAGE_S3_BUCKET`
- `OPENSNOW_STORAGE_GCS_BUCKET`
- `OPENSNOW_STORAGE_AZURE_CONTAINER`
- `OPENSNOW_STORAGE_ACCESS_KEY`
- `OPENSNOW_STORAGE_SECRET_KEY`
- `OPENSNOW_OTEL_DISABLED`

Production account/infra deployments should use workload identity or secret handles instead of inline object-store credentials. CLI output must not print secret values.

## Agent-facing contract

Version: `opensnow-cli.v1`

Required scope for read-only contract discovery: `opensnow:cli:read`

Stable JSON command for agents:

```bash
opensnow cli contract --format json
```

Stable readiness command for agents:

```bash
opensnow cli doctor --format json
```

Related HTTP/API surfaces referenced by the contract:

- `GET /health`
- `GET /status`
- `POST /api/v1/query`
- `POST /api/v1/ingest`
- `POST /api/v1/accounts`
- `POST /api/v1/accounts/{account_id}/workspaces`
