# Security Policy

## Reporting a vulnerability

We take the security of OpenSnow seriously. If you believe you have found a
security vulnerability, please report it to us privately. **Do not open a
public GitHub issue for security problems.**

Email **security@ebdsweden.com** with:

- a description of the issue and the impact you believe it has,
- the affected component or version (commit hash if possible),
- step-by-step reproduction instructions or a proof of concept.

You can expect an acknowledgement within 3 business days and a more detailed
response indicating next steps within 10 business days. We will keep you
informed as we work on a fix and will credit you in the release notes unless
you ask us not to.

## Supported versions

OpenSnow is pre-1.0 software. Until a stable release line is published,
security fixes are applied to the `main` branch only. Pin to a specific
commit or tag and watch releases for security-relevant changes.

## Hardening notes

OpenSnow ships with safe-by-default settings for local development:

- The HTTP/REST and PostgreSQL-wire listeners bind to `127.0.0.1` by default.
  Binding to a non-loopback address while authentication is disabled is
  refused unless you explicitly set `OPENSNOW_ALLOW_PUBLIC=1`.
- JWT authentication is enabled by setting `OPENSNOW_JWT_SECRET` (or the
  enterprise `OPENSNOW_JWT_*` variables). It is off by default for local use.
- The PostgreSQL wire protocol is disabled by default; enable it explicitly
  with `--enable-pgwire` / `[server].pg_enabled = true`.

For any internet-exposed deployment, enable authentication and TLS, run
behind a trusted gateway, and supply secrets from a managed secret store
(see `docs/DEPLOYMENT.md`). Never run with the demo credentials shipped in
the example configuration.
