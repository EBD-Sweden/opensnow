# OpenSnow — SOC 2 Readiness Gap Analysis

**Date:** 2026-06-06
**Scope:** OpenSnow data/SQL platform (Rust workspace, pgwire + REST, dbt integration, Helm/Terraform deploy), positioned as EBD Sweden's managed/hosted data platform.
**Method:** Evidence-based review of the actual codebase (source files, configs, CI, Helm/Terraform). Every claim cites a real path. Assessed against the SOC 2 Trust Services Criteria (TSC 2017, 2022 points of focus).

> **Important framing:** ARCHITECTURE.md describes a target architecture that is substantially more mature than what the code implements. Several controls are documented as present (AES-256-GCM at rest, row-level security, column masking, mTLS, TLS 1.3 in the server) but are **not implemented in the codebase**, or are implemented as design-only contract types / sample SQL. This analysis grades the **code as it exists**, not the blueprint.

---

## Executive verdict

**OpenSnow is NOT SOC 2 ready (Type I or Type II).** It is pre-1.0 software (`SECURITY.md` line 22) with a genuinely promising *security-aware* design, but the gap to an auditable control environment is large. Headline reasons:

1. **No transport encryption is implemented in the product itself.** The server (axum HTTP + pgwire) terminates plaintext; TLS is entirely delegated to an unspecified external proxy. There is no `rustls`/`native-tls` server config anywhere in `crates/`.
2. **"Encryption at rest" is not real cryptography.** The local sealed-secret store uses a hand-rolled XOR keystream (`crates/opensnow-auth/src/secrets.rs::xor_keystream`), not AES-256-GCM. There is no at-rest encryption of warehouse data or the catalog inside the product (only cloud-provider SSE-KMS in the AWS Terraform reference).
3. **The audit log is not tamper-evident.** `audit_events` is a plain auto-increment SQLite table (`crates/opensnow-catalog/src/lib.rs:479`) with no hash chain, WORM, signing, or enforced retention.
4. **Authorization is coarse and brittle.** Object policy is table/database-level only via naive whitespace tokenization (`crates/opensnow-server/src/policy.rs`); the claimed row-level security and column masking do **not** exist. Platform admins bypass all object checks.
5. **SOC 2 is an organizational audit.** There is essentially no organizational control evidence in-repo (no policy set, access-review records, risk assessment, vendor management, change-approval evidence, pen-test, BCP/DR plan, or GRC tooling). Code can satisfy at most a minority of the criteria.

A realistic estimate: **12–18 months** of combined engineering + GRC work before a clean SOC 2 Type II report is achievable for a managed offering.

---

## Maturity summary by Trust Services Criteria

| TSC area | Status | One-line evidence |
|---|---|---|
| CC6.1 Logical access — authN | 🟡 Partial | JWT (HS256/RS256/ES256) + Argon2 client secrets + OIDC sessions exist (`crates/opensnow-server/src/auth.rs`), but auth is **off by default** and pgwire uses cleartext-password-carries-JWT (`crates/opensnow-server/src/pg.rs:578`). |
| CC6.1 Encryption at rest | ❌ Missing | No product-side data/catalog encryption; sealed store uses XOR, not AES (`crates/opensnow-auth/src/secrets.rs:596`). KMS only in AWS Terraform reference. |
| CC6.1 Encryption in transit | ❌ Missing | No TLS server in the binary; only `reqwest` rustls for outbound calls (`crates/opensnow-auth/Cargo.toml:20`). TLS delegated to external proxy. |
| CC6.2 Provisioning / deprovisioning | 🟡 Partial | SCIM user/group lifecycle + token revoke implemented (`crates/opensnow-server/src/auth.rs:469+`); no access-review or recertification process. |
| CC6.3 Least privilege / RBAC | 🟡 Partial | GRANT/REVOKE + scope guards exist; object policy is table-level only, admins bypass, no RLS/column masking (`crates/opensnow-server/src/policy.rs:52`). |
| CC6.6 Boundary protection | 🟡 Partial | Loopback-by-default + `OPENSNOW_ALLOW_PUBLIC` guard (`crates/opensnow-server/src/server.rs:35`); but demo runs `0.0.0.0` + `ALLOW_PUBLIC=1` unauthenticated (`docker-compose.yml:12`). |
| CC6.7 Data in transit between components | ❌ Missing | No mTLS between coordinator/workers (Arrow Flight gRPC is plaintext); ARCHITECTURE.md claims cert-manager mTLS, not in code. |
| CC6.8 Malicious software / integrity | 🟡 Partial | SBOM (CycloneDX) in release; `cargo audit` runs **warn-only** (`.github/workflows/ci.yml:200`); no container/image scanning or signing. |
| CC7.1 Vulnerability detection | 🟡 Partial | `cargo audit` warn-only; no SAST, no scheduled scan, no Dependabot config found. |
| CC7.2 Monitoring / audit logging | 🟡 Partial | Audit events + query history recorded; **not append-only-enforced, not tamper-evident, no SIEM export** (`crates/opensnow-catalog/src/lib.rs:1672`). |
| CC7.3–7.5 Incident response | ❌ Missing | Vulnerability *reporting* mailbox exists (`SECURITY.md:9`); no incident runbooks, on-call, SLAs, or post-incident process in-repo. |
| CC8.1 Change management | 🟡 Partial | CI lint/test/build/Helm-lint (`.github/workflows/ci.yml`); no documented branch protection, CODEOWNERS, mandatory review, or change-approval evidence. |
| A1.1–A1.3 Availability / BCP-DR | 🟡 Partial | K8s liveness/readiness probes, KEDA autoscale, AWS RDS 7-day backups + S3 versioning (`deploy/terraform/README.md:148`); **no defined RTO/RPO, no DR test, no rate limiting**. |
| PI1 Processing integrity | 🟡 Partial | Iceberg ACID/idempotent commits, dbt test gating possible, query timeout + concurrency cap; no formal data-validation/reconciliation controls. |
| C1 Confidentiality | 🟡 Partial | Secret-handle abstraction + scoped tokens; no data classification engine, no masking, no field-level confidentiality controls in code. |
| P-series Privacy / GDPR | ❌ Missing | GDPR erasure/retention are **sample SQL in demo datasets only** (`crates/opensnow-industry/src/banking/compliance.rs:139`), not platform features. |
| CC1–CC5 Org / governance | ❌ Missing | No policy set, risk assessment, org chart, vendor register, or control evidence in-repo. |

Legend: ✅ in place · 🟡 partial · ❌ missing.

---

## Per-area detail

### 1. Security / Access control (CC6.1–6.3, CC6.6–6.8)

**What exists**
- **JWT auth** with three modes: local HS256 (`OPENSNOW_JWT_SECRET`), and asymmetric enterprise RS256/ES256 with `iss`/`aud`/`kid`, JWKS publication, rotated verify-only keys, and revoked-`kid` support (`crates/opensnow-server/src/auth.rs:1509`). Fails closed on wrong issuer/audience/expiry.
- **Service-client credentials**: `client_id:client_secret` with **Argon2id** hashing, scopes, tenant binding, suspend/revoke, expiry, evaluation quotas (`crates/opensnow-server/src/auth.rs:101-400`).
- **OIDC** durable SSO sessions re-validated on each request (`crates/opensnow-server/src/auth.rs:427` per ARCHITECTURE 10.1).
- **Scope-based route guards**: `require_query_scope`, `require_ingest_*_scope`, `require_admin_scope`, `require_audit_read_scope` layered per route (`crates/opensnow-server/src/auth.rs:1362-1394`, wired in `rest.rs:158+`).
- **Tenant isolation**: handlers read tenant from the verified `AuthContext`, not client headers; `X-Tenant-ID` mismatch is rejected (`crates/opensnow-server/src/auth.rs` token tenant check). Query history is recorded per tenant.
- **Network-exposure guard**: refuses to bind a non-loopback address with auth disabled unless `OPENSNOW_ALLOW_PUBLIC=1` (`crates/opensnow-server/src/server.rs:35`). pgwire disabled by default (`server.rs:12`).
- **Object policy** enforced uniformly across REST/pgwire/dbt/MCP via `ObjectPolicyStore::check_sql` (`crates/opensnow-server/src/policy.rs:51`); fails closed on unparseable SQL.

**What is missing / weak**
- **Auth is OFF by default** (`SECURITY.md:33`, ARCHITECTURE 10 "Localhost mode: no auth"). For a *managed/hosted* product this is the opposite of the SOC 2 default-deny posture; the shipped `docker-compose.yml` runs fully unauthenticated on `0.0.0.0` with `OPENSNOW_ALLOW_PUBLIC=1`. **Severity: High** (Critical if this compose is used as a hosting template).
- **Authorization is coarse and parser-fragile.** `policy.rs` tokenizes SQL with a hand-rolled char scanner (`tokenize_sql`, line 289) and keyword matching — not a real parser. It extracts only `FROM`/`JOIN`/`INTO` table names at the top level; subqueries in unusual positions, CTEs referenced indirectly, functions/UDFs, `MERGE`, `COPY`, and many statements are not modeled and are denied or mis-evaluated. **Severity: High.**
- **Claimed RLS and column masking do not exist.** ARCHITECTURE 10.2 lists "Row-level security (policy-injected WHERE)" and "Column-level masking" — there is no such code. Grep for mask/row-level returns only sample-data files. **Severity: High** for a confidentiality-sensitive warehouse.
- **Platform admins bypass all object checks** (`policy.rs:52`) and pgwire auth lets any valid bearer subject not in the client registry through (`auth.rs:330-339` `authorize_bearer_client` returns `Ok` for unknown clients). **Severity: Medium.**
- **pgwire authN is cleartext-password-over-the-wire** carrying the JWT, with no SCRAM and no in-product TLS (`pg.rs:570`, ARCHITECTURE 7.4). Safe only over loopback/port-forward/external TLS. **Severity: High** if exposed.
- **No mTLS between coordinator and workers**; Arrow Flight/gRPC shuffle is plaintext (`crates/opensnow-distributed/`). **Severity: High** for multi-node.
- **Secret management**: production resolvers (AWS Secrets Manager, GCP, Vault) shell out to the provider CLIs and fail closed (`crates/opensnow-auth/src/secrets.rs:210`). Reasonable for a reference, but shelling to `aws`/`vault`/`gcloud` binaries is fragile and adds supply-chain/exec surface. **Severity: Medium.**
- **Default credentials in shipped artifacts**: `OPEN-SNOW-DEMO-ONLY` MinIO/Grafana passwords and `admin` Grafana user (`docker-compose.yml:27,55`). Clearly demo-scoped, but must never reach a hosted env. **Severity: Medium.**

### 2. Encryption (CC6.1, C1)

**What exists**
- Outbound HTTP uses rustls via `reqwest` (`crates/opensnow-auth/Cargo.toml:20`).
- AWS BYOC Terraform provisions SSE-KMS S3 warehouse storage, KMS, and Object-Lock audit bucket (`deploy/terraform/README.md:33`).
- Helm gates: enterprise/BYOC renders **fail** unless `enterprise.tls.enabled=true` + `tls.existingSecret`, and pgwire exposure requires TLS (`deploy/helm/opensnow/templates/configmap.yaml:57-67`). Postgres metadata DSN uses `sslmode=require` for external (`_helpers.tpl:106`).

**What is missing / weak**
- **In transit:** No TLS listener in the OpenSnow binary. The Helm `tls.enabled` flag is a *render-time guard*, not server TLS — actual encryption depends on an external ingress/NLB/proxy the operator must supply and configure correctly. There is no enforcement that traffic to the pod is encrypted. **Severity: Critical** for a managed offering.
- **At rest:** No encryption of warehouse Parquet, the SQLite/Postgres catalog, or local caches by the product. The "sealed" local secret store uses `xor_keystream` (XOR of plaintext ⊕ key ⊕ nonce) — **not** a real cipher and trivially breakable; ARCHITECTURE 10.3 claims "AES-256-GCM envelope encryption." **Severity: Critical** (also a documentation-accuracy/control-misrepresentation issue).
- **Key management / rotation:** JWT key rotation exists; data-key rotation, KMS envelope encryption inside the product, and master-key lifecycle do not. **Severity: High.**

### 3. Audit logging (CC4.1, CC7.2)

**What exists**
- Structured `AuditEvent` envelope (actor, auth method, action, resource, result, trace id, secret-handle refs, redacted metadata) — `AuditEventBuilder` / `AuditEvent` in `crates/opensnow-auth`.
- Events appended for SCIM ops, pgwire allow/deny (`pg.rs:283`), and policy decisions; `append_audit_event` + `search_audit_events` (`crates/opensnow-catalog/src/lib.rs:1672-1689`). Per-tenant query history recorded.
- Audit export is scope-gated (`audit.read`/`policy.admin`).

**What is missing / weak**
- **Not tamper-evident.** `audit_events` is a mutable SQLite table keyed by autoincrement (`lib.rs:479`); no hash chaining, no signatures, no WORM, and a DB admin can `UPDATE`/`DELETE` rows. The test named `..._append_only_...` only asserts query scoping, not immutability. **Severity: High** (CC7.2 evidence integrity).
- **No retention or export to SIEM.** No retention policy, no log-shipping, no immutable sink. (AWS reference has an Object-Lock *bucket* but no code path that writes audit there.) **Severity: High.**
- **Coverage gaps.** REST `execute_query` audit coverage is less explicit than pgwire's; admin/data-movement routes (`/tables/register`, `/export/postgres`) audit coverage should be verified. **Severity: Medium.**

### 4. Availability (A1.1–A1.3)

**What exists**
- K8s liveness/readiness probes (`deploy/helm/opensnow/templates/coordinator-deployment.yaml:183`), KEDA autoscaling (`deploy/keda-scaledobject.yaml`).
- Process-wide admission control: `OPENSNOW_MAX_CONCURRENT_QUERIES` semaphore (default 4, max 64) and `OPENSNOW_QUERY_TIMEOUT_SECS` (default 30, max 300) — ARCHITECTURE 5.2.
- AWS reference: RDS 7-day automated backups + deletion protection, S3 versioning/replication guidance (`deploy/terraform/README.md:148`).

**What is missing / weak**
- **No HTTP rate limiting / per-client throttling** (only a global query semaphore). No `tower_governor` or equivalent. **Severity: Medium.**
- **No defined RTO/RPO, no DR runbook, no restore test.** Backups exist only in the AWS reference; the default/local/MinIO and self-hosted Hetzner/OCI paths have no backup strategy. **Severity: High** for a managed offering.
- **Catalog backup is operator responsibility** with no tooling beyond `reset-runtime-state` (ARCHITECTURE 6.5). **Severity: Medium.**
- **No multi-region/HA story** beyond stateless replicas; metadata DB is a single point. **Severity: Medium.**

### 5. Processing Integrity (PI1)

**What exists**
- Iceberg v2 ACID snapshots and idempotent commits (ARCHITECTURE 4.1, 8.3) give exactly-once-ish ingestion semantics.
- dbt integration enables `dbt test` gating in pipelines (`crates/opensnow-server/src/dbt.rs`, `integrations/dbt-opensnow/`).
- Catalog migrations are idempotent with a version row and storage preflight (ARCHITECTURE 6.5).
- Query timeout + concurrency caps bound runaway processing.

**What is missing / weak**
- **No enforced input/schema validation framework** for ingested data beyond format parsing; no reconciliation/row-count controls as a platform feature. **Severity: Medium.**
- **dbt test gating is opt-in**, not a controlled gate with evidence. **Severity: Low–Medium.**
- **pgwire is simple-query-only**; extended protocol, COPY, broad `pg_catalog` are unsupported (`pg.rs:623`), which is an integrity/compatibility caveat for BI tools that assume PG semantics. **Severity: Low** (functional, not a control gap).

### 6. Confidentiality / Privacy (C1, P-series)

**What exists**
- Secret-handle abstraction keeps raw secrets out of API/list responses and logs (`SecretValue` redacts Debug, no Serialize — `secrets.rs:106`).
- Scoped, tenant-bound tokens limit data reach.

**What is missing / weak**
- **No data classification, tagging, or sensitivity engine** in code (ARCHITECTURE 10.4 `SET TAG sensitivity='PII'` not implemented). **Severity: High** for a data platform.
- **No PII masking / dynamic masking** (see RLS gap above). **Severity: High.**
- **GDPR deletion/export/retention are not platform features** — they are illustrative SQL string builders inside `opensnow-industry` sample datasets (`crates/opensnow-industry/src/banking/compliance.rs:139`, `telecom/compliance.rs:11`). There is no DSAR workflow, no verified erasure, no retention enforcement engine. **Severity: High** for any EU-customer managed offering.
- **No data-residency controls** in the product (telecom sample has a `data_residency_check` helper only). **Severity: Medium.**

### 7. Change management & supply chain (CC8.1)

**What exists**
- CI: rustfmt + clippy (`-D warnings`), workspace tests on Linux+macOS, release build, quickstart smoke, Helm lint/template, duplicate-crate gate (`.github/workflows/ci.yml`).
- Dependency hygiene: `cargo machete` (unused deps) and `cargo audit` (CVEs) in CI and `scripts/dep-check.sh`.
- Release: multi-arch Docker to GHCR, multi-target binaries with SHA-256 checksums, **CycloneDX SBOM** generated and attached (`.github/workflows/release.yml:127`).
- Reproducible Dockerfile runs as non-root `USER opensnow` uid 1000 (`Dockerfile:9,17`).

**What is missing / weak**
- **`cargo audit` is `continue-on-error: true`** (`ci.yml:200`) — known CVEs do **not** block merge or release. **Severity: High.**
- **No SAST** (no CodeQL/Semgrep), **no container image scanning** (Trivy/Grype), **no secret scanning** (gitleaks/trufflehog) in CI. **Severity: High.**
- **No image signing.** DEPLOYMENT.md tells users to `cosign verify` (`docs/DEPLOYMENT.md:452`), but the release workflow has **no cosign/attestation step** — images are unsigned. This is a documentation-vs-implementation gap. **Severity: High.**
- **No documented branch protection, CODEOWNERS, required reviews, or signed commits** discoverable in-repo. SOC 2 CC8.1 needs evidence that changes are reviewed/approved/tested before production. **Severity: High** (organizational).
- SBOM is for the Rust binary only, not the full container image. **Severity: Medium.**

### 8. Monitoring & incident response (CC7.3–7.5)

**What exists**
- Prometheus metrics registry + `/metrics` endpoint (`crates/opensnow-server/src/metrics.rs`), per-warehouse/query counters.
- OpenTelemetry tracing scaffolding (`crates/opensnow-server/src/telemetry.rs`) — note: default exporter is **stdout**, OTLP only when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
- Prometheus + Grafana in docker-compose and Helm; `/health` unauthenticated for probes (`rest.rs:149`).

**What is missing / weak**
- **No alerting rules** shipped (Prometheus config has no alert rules / Alertmanager). **Severity: Medium.**
- **No incident-response runbooks, on-call, severity definitions, comms plan, or post-incident review** in-repo (only a vuln-report mailbox in `SECURITY.md`). **Severity: High** (organizational, CC7.3–7.5).
- **No centralized log aggregation / SIEM** integration. **Severity: Medium.**

### 9. Organizational reality (CC1–CC5, vendor mgmt, evidence)

SOC 2 is fundamentally an **audit of the organization's control environment over time**, not a code certification. From the repository, essentially none of the following exist and **cannot be produced by code**:
- Security/acceptable-use/access-control/change-management/incident/BCP-DR/data-retention **policies** and management approval.
- **Risk assessment**, control matrix, and control owners (CC3).
- **Access reviews / recertification** evidence (CC6.2/6.3).
- **Vendor / sub-processor management** (AWS, GHCR, MinIO, Grafana Cloud, etc.) (CC9.2).
- **Background checks, security training, HR onboarding/offboarding** (CC1.4).
- **Independent penetration test** and remediation evidence.
- **GRC tooling** (Vanta/Drata/Secureframe) to collect continuous evidence.
- A **Type I** (design at a point in time) then **Type II** (operating effectiveness over a 3–12 month window) engagement with a licensed CPA firm.

---

## Technical gaps vs. organizational gaps

**Technical (code-fixable in this repo)**
- In-product TLS for HTTP + pgwire; mTLS between distributed components.
- Real at-rest encryption (AES-256-GCM envelope + KMS) for catalog/data; replace `xor_keystream`.
- Tamper-evident audit log (hash chain / signed segments / append-only sink) + retention + SIEM export.
- Robust authorization: parser-based policy analysis, row-level security, column masking, data classification/tagging.
- HTTP rate limiting; backup/restore tooling for the catalog; OTLP alerting rules.
- CI: make `cargo audit` blocking; add SAST, container scan, secret scan, image signing (cosign) + attestations; full-image SBOM.
- GDPR DSAR/erasure/retention as real platform features (not sample SQL).

**Organizational (process/policy/audit — not code)**
- The full SOC 2 policy set, risk assessment, and control matrix with owners.
- Access reviews, change-approval evidence, branch protection + required reviews.
- Vendor/sub-processor management, HR security controls, security training.
- Incident-response program (runbooks, on-call, SLAs, PIRs).
- BCP/DR plan with tested RTO/RPO.
- Independent pen test; GRC tooling; auditor engagement (Type I → Type II).

---

## Prioritized remediation backlog

### P0 — Blockers for any managed/hosted exposure (do first)
| Action | Why | Rough effort |
|---|---|---|
| Implement TLS for HTTP and pgwire in the binary (rustls), enforced for non-loopback; default-deny plaintext for hosted mode | CC6.1/6.7 in-transit encryption is currently absent | 2–4 wks |
| Replace `xor_keystream` with AES-256-GCM; add KMS-backed envelope encryption for catalog + at-rest data; document key lifecycle | CC6.1 at-rest + fixes a misrepresented control | 3–6 wks |
| Make auth **default-on** for any non-loopback bind; remove `ALLOW_PUBLIC` unauthenticated path from hosting templates; rotate all demo creds | CC6.1/6.6 default-deny | 1–2 wks |
| Tamper-evident, append-only audit log (hash chain or signed/Object-Lock sink) + retention + SIEM export | CC7.2 evidence integrity (core SOC 2 requirement) | 3–5 wks |
| Make `cargo audit` blocking; add secret scanning, SAST (CodeQL), container scan (Trivy), and **cosign image signing** (matching DEPLOYMENT.md) | CC6.8/7.1/8.1 supply chain | 1–2 wks |

### P1 — Required for Type I readiness
| Action | Why | Rough effort |
|---|---|---|
| Parser-based authorization (use the existing sqlparser/DataFusion plan); implement row-level security + column masking | CC6.3/C1 — close the largest "documented but absent" gap | 6–10 wks |
| Data classification/tagging + masking-by-tag | C1 confidentiality | 4–6 wks |
| Backup/restore tooling for the catalog across all deploy modes; define + test RTO/RPO | A1 availability | 3–5 wks |
| mTLS between coordinator/workers (cert-manager) | CC6.7 | 2–4 wks |
| HTTP rate limiting + per-tenant quotas | A1/CC6.6 | 1–2 wks |
| Author the SOC 2 policy set, risk assessment, control matrix; enable branch protection + CODEOWNERS + required review; stand up GRC tooling | CC1–CC5/CC8 organizational foundation | 4–8 wks (parallel, GRC-led) |
| Incident-response program: runbooks, severities, on-call, PIR template; Prometheus alert rules | CC7.3–7.5 | 2–4 wks |

### P2 — Maturity / Type II hardening
| Action | Why | Rough effort |
|---|---|---|
| Real GDPR DSAR/erasure/retention platform features replacing sample SQL | P-series privacy | 4–8 wks |
| Full-container SBOM + provenance attestations (SLSA) | CC8.1 supply chain | 1–2 wks |
| Centralized log aggregation, retention, and access controls | CC7.2 | 2–4 wks |
| Access-review automation + quarterly recertification evidence | CC6.2/6.3 | ongoing |
| Independent penetration test + remediation | CC4/auditor expectation | external |

---

## Realistic path to SOC 2

1. **Foundation (months 0–3):** Land all **P0** items + start the org/GRC track (policies, risk assessment, control matrix, GRC tooling, branch protection). Decide audit scope (Security TSC mandatory; add Availability + Confidentiality given the managed data-platform positioning; Privacy/Processing Integrity optional but likely expected for EU data).
2. **Type I readiness (months 3–6):** Complete **P1** technical controls and have the policy/control environment *designed and in place*. Engage a CPA firm for a **Type I** (design assessment at a point in time). Run a pre-audit readiness assessment / mock audit.
3. **Type II observation window (months 6–15):** Operate the controls continuously, collecting evidence (access reviews, change approvals, monitoring/alerting, incident records, backup/restore tests). Land **P2** items. Commission an independent **penetration test**. Typical window is **3–12 months**; 6 months is common for a first Type II.
4. **Type II audit (months 12–18):** Auditor tests operating effectiveness across the window and issues the **SOC 2 Type II** report. Thereafter, run a continuous annual cycle.

**Bottom line:** the code shows real security awareness (Argon2, scoped JWTs, fail-closed policy, loopback-by-default, SBOM, non-root container), but the platform is far from auditable: the two most fundamental data-protection controls (encryption in transit and at rest) are not actually implemented in the product, the audit trail is mutable, and the entire organizational control environment that SOC 2 actually certifies does not yet exist.

---

### Notes / areas that were unclear

- I read the primary auth/pgwire/policy/secrets/catalog/CI/deploy files directly. `crates/opensnow-server/src/auth.rs` is ~5k lines; I reviewed the first ~1640 lines plus targeted greps — some OIDC/SSO and admin-route detail beyond that range is described from ARCHITECTURE.md and grep, not full line-by-line reading.
- I did **not** execute the code or run the test suite; control *operating effectiveness* (vs. presence) cannot be asserted from static review.
- Several Terraform `.terraform/modules/...` paths are vendored upstream modules (AWS EKS), not OpenSnow code; I treated them as deploy-reference only.
