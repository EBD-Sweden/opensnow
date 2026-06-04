# OpenSnow

OpenSnow is an open-source analytics warehouse built with Rust, Apache DataFusion, Iceberg/Parquet, object storage, PostgreSQL wire compatibility, Axum APIs, and Kubernetes-native deployment primitives. It aims to be a self-hostable, Snowflake-style analytics engine.

> **Status:** OpenSnow is pre-1.0, actively developed software. Expect rough edges and breaking changes. It is suitable for evaluation and self-hosted experimentation; do not treat it as enterprise-auth ready until you have validated the auth/TLS guidance in `docs/DEPLOYMENT.md` and `SECURITY.md`.

## Prerequisites

- A recent stable Rust toolchain — the repository pins one via `rust-toolchain.toml`, so `rustup` installs it automatically (edition 2024).
- `python3` (used by the demo and smoke scripts).
- Optional: Docker / `docker compose` for the containerized demo, `kubectl` + Helm for Kubernetes.

## Try OpenSnow in 10 minutes

Safest one-command public demo from a fresh clone:

```bash
scripts/demo.sh
```

The demo command starts local OpenSnow if needed, loads the deterministic synthetic manifest at `demo/public-demo-manifest.json`, runs REST smoke checks, and prints next steps. Reset local demo state with:

```bash
scripts/demo.sh reset
```

Manual smoke script for an already-running server:

```bash
scripts/public-smoke.sh
```

The smoke script validates health/status and REST ingest/query by default. PostgreSQL wire is disabled by default for public-demo safety; to run trusted local pgwire checks with the one-command demo, use `OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/demo.sh`. For an already-running server, start with `cargo run -p opensnow-cli -- start --enable-pgwire` and run `OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/public-smoke.sh`.

Full external-user guide: `docs/PUBLIC_TEST_PATH.md`. One-command demo details: `docs/PUBLIC_DEMO.md`.

## Main entry points

- Architecture: `ARCHITECTURE.md`
- Deployment guide: `docs/DEPLOYMENT.md`
- OpenSnow CLI: `docs/CLI.md`
- SQL compatibility: `docs/SQL_COMPATIBILITY.md`
- Public test path: `docs/PUBLIC_TEST_PATH.md`
- One-command public demo: `docs/PUBLIC_DEMO.md`
- Security policy: `SECURITY.md`
- Contributing: `CONTRIBUTING.md`

## Deployment safety

Local and Docker/k3d demos are the intended quickstart path and bind to loopback by default. For any internet-exposed deployment, enable authentication and TLS, run behind a trusted gateway, and supply secrets from a Kubernetes/cloud secret manager — never the demo credentials shipped in the example configuration. See `docs/DEPLOYMENT.md` and `SECURITY.md`.

## License

OpenSnow is licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for attribution.

"Snowflake" is a trademark of Snowflake Inc. OpenSnow is an independent open-source project and is **not** affiliated with, endorsed by, or sponsored by Snowflake Inc.; references to Snowflake describe interoperability and comparison only.
