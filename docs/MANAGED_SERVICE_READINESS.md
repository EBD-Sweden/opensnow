# OpenSnow Managed-Service Readiness & Gap-Closure Plan

**Goal:** turn OpenSnow (Rust analytics warehouse) into a sellable **managed**
offering on AWS and GCP — "we run Kubernetes + object storage on the customer's
behalf" — alongside the existing **BYOC** (bring-your-own-cloud / customer-account
install) shape.

**Method:** every claim below was verified by reading the code on branch `main`
and is cited as `path:line`. Effort sizes are **S** (≤1 wk, 1 eng), **M**
(1–3 wk), **L** (3–8 wk). This document changes no code.

> **Scope note on "managed":** Two distinct managed shapes exist and the gaps
> differ. **(a) BYOC managed** = Terraform + Helm land in the *customer's* AWS/GCP
> account, we operate it. **(b) Multi-tenant SaaS** = customers share our cluster
> and buckets. The current codebase is built for (a) and for single-tenant
> enterprise installs; true (b) multi-tenant pooling is the largest lift and is
> deliberately staged to Phase 3.

---

## 1. Executive summary

OpenSnow is **further along than a typical "demo"**: the data plane (distributed
coordinator/worker over Arrow Flight), the infra-as-code (AWS EKS + GCP GKE
Autopilot, encrypted versioned buckets, IRSA / Workload Identity), the Helm chart
(coordinator HPA + worker StatefulSet + KEDA autoscaling + render-time fail-gates),
and **substantial enterprise-auth machinery** (RS256/ES256 JWT validation, JWKS
publication, multi-tenant OIDC code-exchange, a full SCIM 2.0 user/group/token
surface, a hash-chained audit log, and entitlement-gated warehouse activation) are
all real and largely test-covered.

The honest blockers to selling a **managed** service are concentrated in **runtime
wiring and one missing daemon**, not in missing primitives:

1. **No Kubernetes operator daemon** — reconcile *logic* and a `kubectl`-based
   apply loop exist, but nothing **watches the `Warehouse` CRD**; scaling is a
   manual CLI call.
2. **Marketplace entitlement is never enforced at runtime** — the enforcement
   *function* exists and is tested, but no server code fetches the entitlement
   (Terraform only writes it to SSM as a "hint") and feeds it to the gate.
3. **Audit export is config-only** — the object-locked bucket is provisioned and
   an "audit_export configure" endpoint persists settings, but the hash-chained
   audit log lives in SQLite and is **never shipped to S3/GCS**.
4. **OIDC code exchange exists but is fenced off** in the embedded admin SSO path
   (fails closed by design).
5. **Sealed-secret sync and RDS rotation are unimplemented** — Helm references
   secret providers; nothing syncs them or rotates the DB password.
6. **Flight data path is incomplete** — `do_get`/`do_put`/`do_exchange` are
   `unimplemented`; there is no per-partition retry / fault tolerance.
7. **Multi-tenant pooling does not exist** — there is per-account *logical*
   isolation in the catalog and entitlement model, but no shared-cluster quotas,
   noisy-neighbor controls, or per-tenant usage metering/billing.

### Readiness verdict per offering

| Offering | Verdict | One-line rationale |
|---|---|---|
| **AWS BYOC install** (customer account, single tenant) | 🟡 **Sellable with a pilot wrapper** | Terraform + Helm apply-and-run; auth/JWT/SCIM real; needs runtime entitlement wiring removed-or-bypassed, secret-sync runbook, and the operator only if multi-warehouse autoscaling is promised. |
| **GCP BYOC install** | 🟡 **Sellable with a pilot wrapper** | Same as AWS; GCP Terraform is thinner (GKE Autopilot + GCS + WI) and lacks the audit-export / KMS / SSM-hint parity AWS has. |
| **AWS managed SaaS** (we operate) | 🔴 **Not yet** | Needs operator daemon, runtime entitlement enforcement, audit export, secret-sync, and tenant quotas/metering. |
| **GCP managed SaaS** | 🔴 **Not yet** | Same as AWS plus GCP Terraform parity work. |

**Bottom line:** BYOC is a **one-quarter** finish. Managed SaaS is **two
quarters** with multi-tenant pooling/metering being the long pole.

---

## 2. What works today (verified)

- **Terraform AWS** — EKS + VPC; KMS-encrypted (`aws_s3_bucket_server_side_encryption_configuration.warehouse`, `deploy/terraform/main.tf:120`) and versioned (`main.tf:98`) warehouse bucket (`main.tf:93`); public-access blocked (`main.tf:130`); IRSA policy scoped to the bucket (`main.tf:180`, `:191`); **object-locked** audit-export bucket (`object_lock_enabled = true`, `main.tf:242`; lock config `main.tf:252`); optional RDS; SSM "enterprise helm hints" parameter (`main.tf:335`).
- **Terraform GCP** — GKE Autopilot + GCS + Workload Identity (`deploy/terraform/gcp/main.tf`, 152 lines; thinner than AWS — no object-locked audit bucket, no SSM-equivalent hint store, no KMS module parity).
- **Helm chart** — coordinator Deployment with JWT env wiring (`deploy/helm/opensnow/templates/coordinator-deployment.yaml:95`), worker StatefulSet, ServiceAccount with IRSA/WI annotations (`templates/serviceaccount.yaml`), **KEDA** ScaledObject (`templates/keda.yaml`), NetworkPolicy, and **render-time fail-gates** that reject enterprise renders without a valid secret provider (`templates/configmap.yaml:51`).
- **Distributed engine** — real coordinator (`crates/opensnow-distributed/src/coordinator.rs`), worker that registers/heartbeats over Flight `do_action` (`worker.rs:101`,`:106`,`:177`), scheduler (`scheduler.rs`), and a `DistributedExecutor` that fans fragments across `WorkerExecutor`s with a `LocalExecutor` fallback (`distributed_executor.rs:46`,`:97`,`:127`).
- **Enterprise auth (larger than expected):**
  - JWT: enterprise RS256/ES256 keys, `validate_token` with `iss`/`aud`/`exp` checks and JWKS-style key rotation (`crates/opensnow-auth/src/jwt.rs:219`,`:424`,`:461`); enforced in pgwire (`crates/opensnow-server/src/pg.rs:233`) and HTTP bearer middleware (`crates/opensnow-server/src/auth.rs:1706`,`:1720`).
  - Multi-tenant OIDC: real discovery + code exchange over HTTP (`crates/opensnow-auth/src/sso.rs:1081 exchange_oidc_code`, `:1010 verify_oidc_token`, `:1141 complete_oidc_code_login`).
  - SCIM 2.0: users/groups/tokens with routed endpoints (`crates/opensnow-server/src/auth.rs:3503` router; `:3445`,`:3506`,`:3517`).
  - Audit: hash-chained, append-only audit log with chain verification (`crates/opensnow-catalog/src/lib.rs:1774 append_audit_event`, `:1814 verify_audit_chain`, table at `:550`).
  - Entitlements: enforcement model + warehouse-activation gate that denies and audits on inactive/missing-feature entitlements (`crates/opensnow-auth/src/contract.rs:527 EntitlementCheck`; `crates/opensnow-catalog/src/lib.rs:982`–`:1001`).

---

## 3. Gap table

| # | Gap | Current state (file:line) | What to build | Effort | Blocks |
|---|---|---|---|---|---|
| 1 | **No operator daemon watching CRDs** | CRD exists (`deploy/crds/warehouses.opensnow.io.yaml`); reconcile logic `build_reconcile_plan` (`crates/opensnow-distributed/src/operator.rs:152`); a `kubectl`-shell apply loop `run_reconcile_loop` exists (`crates/opensnow-distributed/src/k8s.rs:181`) but **is never called** (only `OperatorApply` CLI one-shot `crates/opensnow-cli/src/main.rs:1247`,`:1283`). No `kube` crate dependency anywhere. | Add `kube`/`kube-runtime`; implement a `Controller` watching `Warehouse` CRDs + worker StatefulSets, feeding existing `build_reconcile_plan`; replace `kubectl` shell with typed `Api<StatefulSet>` patches; add `--role operator` server entrypoint. | **L** | AWS/GCP managed SaaS; multi-warehouse BYOC autoscaling |
| 2 | **Marketplace entitlement not enforced at runtime** | Gate is real & tested (`catalog/src/lib.rs:982`); but `create_enterprise_warehouse` / `EntitlementCheck::new` are **not called from server or core** (only catalog tests). Terraform writes `marketplace_enabled`/`marketplace_entitlement` only to an SSM hint (`terraform/main.tf:349`-`:350`); no code reads SSM. | Build a runtime entitlement resolver (AWS Marketplace Metering/Entitlement API + GCP Procurement API, or read the SSM/Secret hint), construct `EntitlementCheck`, and pass it into the activation path; cache + periodic refresh; fail-closed when `entitlement_required`. | **M** | AWS/GCP managed SaaS; marketplace listings |
| 3 | **Audit export is config-only (not shipped)** | Object-locked bucket provisioned (`terraform/main.tf:239`); "configure" endpoint persists settings to catalog (`server/src/auth.rs:2477`); audit log stored in **SQLite** (`catalog/src/lib.rs:550`); **no `put_object` / S3 SDK call for audit anywhere**. | Build an audit-export worker: stream `search_audit_events` deltas to the object-locked bucket (Parquet/JSONL), checkpoint cursor, verify chain before upload, emit export-success audit events; wire bucket name from SSM hint / Helm value. | **M** | Compliance posture for managed + marketplace; SOC2 story |
| 4 | **OIDC code exchange fenced off in admin path** | Library exchange works (`sso.rs:1081`), but embedded admin SSO **fails closed**: "backend code exchange/session token minting is not enabled; raw authorization codes fail closed" (`server/src/admin.rs:313`). | Wire `complete_oidc_code_login` into the admin callback, mint SSO session tokens via `generate_sso_session_token` (`jwt.rs:346`), add CSRF/state/nonce/PKCE verification end-to-end, and feature-flag per tenant. | **M** | End-to-end SSO login for managed tenants (JWT API auth already works without it) |
| 5 | **Sealed-secret sync & RDS rotation absent** | Helm references providers `aws-secrets-manager`/`gcp-secret-manager`/`vault` and gates renders (`helm/.../configmap.yaml:51`, `values-enterprise-*.yaml:57`), but **nothing syncs provider secrets into k8s Secrets** and there is no DB-password rotation loop. RDS secret ARN only appears as an SSM hint (`terraform/main.tf:351`). | Adopt External Secrets Operator or Secrets Store CSI in the chart (SecretProviderClass + ExternalSecret manifests); add a rotation reconciler (or rely on RDS-managed rotation + restart hook). Document as an operational runbook for BYOC. | **M** | Managed SaaS (we hold the keys); hardens BYOC |
| 6 | **Flight data path incomplete; no fault tolerance** | `do_get`/`do_put`/`do_exchange` are `Status::unimplemented` (`coordinator.rs:172`,`:179`,`:190`); worker only uses `do_action` for register/heartbeat (`worker.rs:106`); executor doc admits "no per-partition retry yet" (`distributed_executor.rs:17`). | Implement Flight `do_get`/`do_put` for partition result streaming; add per-fragment retry + reassignment on worker loss; promote `RemoteWorkerExecutor` over Flight. | **L** | Large-scale managed workloads (small workloads run via local fallback today) |
| 7 | **No multi-tenant pooling / quotas / metering** | Per-account *logical* isolation exists in catalog (account/org scoping on audit search `catalog/src/lib.rs:1878`, entitlement model) and a `tenant_middleware` (`server/src/tenant.rs:46`); but **no shared-cluster quotas, concurrency caps, noisy-neighbor controls, or per-tenant usage metering/billing**. | For pooled SaaS: per-tenant warehouse namespaces/quotas via the operator, query concurrency + resource limits, and a usage-metering pipeline (CPU-seconds / bytes-scanned → billing). Until then, sell **single-tenant-per-cluster** managed. | **L** | Multi-tenant SaaS only (single-tenant managed unaffected) |

---

## 4. Phased roadmap

### Phase 1 — "BYOC GA" (sell a confident customer-account install)
**Theme:** make the apply-and-run path trustworthy, documented, and enforceable
for a *single-tenant* customer install we operate or co-operate.

**Workstreams & owners-by-crate**
- **Entitlement wiring (lite)** *(opensnow-server, opensnow-core)* — Gap #2 minimal slice: resolve entitlement from the SSM/Secret hint and feed `EntitlementCheck` into activation; for non-marketplace BYOC, allow `entitlement_required=false` to bypass cleanly. **M**
- **Secret-sync runbook + chart manifests** *(deploy/helm)* — Gap #5: ship External-Secrets/CSI `SecretProviderClass`+`ExternalSecret` templates and a rotation runbook. **M**
- **GCP Terraform parity** *(deploy/terraform/gcp)* — add object-locked audit bucket, KMS/CMEK, and a hint store (Secret Manager) to match AWS. **M**
- **SSO end-to-end (optional for GA)** *(opensnow-server/admin, opensnow-auth)* — Gap #4 if the pilot needs browser SSO; JWT API auth already suffices otherwise. **M**
- **Install validation harness** *(deploy, opensnow-cli)* — extend existing `cli` readiness checks (`opensnow-core/src/cli.rs:80`,`:348`) into a post-apply smoke test (deploy → auth → query → suspend/resume).  **S**

**Exit criteria**
- `terraform apply` (AWS *and* GCP) + `helm install` on a clean account yields a working coordinator/worker that authenticates a JWT and runs a query, verified by the smoke harness.
- Secrets resolve from the cloud provider into pods with no plaintext in values.
- For non-marketplace installs, warehouse activation succeeds without entitlement friction; for marketplace installs, an inactive entitlement is denied **and audited** at runtime (not just in tests).
- A signed runbook covers secret rotation, backup/restore of the catalog DB, and upgrade.

### Phase 2 — "Managed SaaS MVP" (we operate it; single-tenant-per-cluster)
**Theme:** the daemon + enforcement that let us run it without humans clicking
`kubectl scale`, billing accurately, and proving entitlement.

**Workstreams & owners-by-crate**
- **Operator daemon** *(opensnow-distributed, new bin in opensnow-server or opensnow-cli)* — Gap #1: `kube`-based `Controller` watching `Warehouse` CRDs + StatefulSets, reusing `build_reconcile_plan`; `--role operator` entrypoint; replace shell `kubectl` with typed API. **L**
- **Runtime entitlement enforcement (full)** *(opensnow-server, opensnow-auth)* — Gap #2: AWS Marketplace Metering/Entitlement + GCP Procurement integration, periodic refresh, fail-closed activation + per-feature gating. **M**
- **Audit export pipeline** *(opensnow-server, opensnow-catalog)* — Gap #3: ship chain-verified audit batches to the object-locked bucket with checkpointing. **M**
- **Per-tenant guardrails (single-tenant baseline)** *(opensnow-server/tenant, operator)* — concurrency caps and resource limits per warehouse via operator-set StatefulSet resources. **M**

**Exit criteria**
- Creating/suspending a `Warehouse` CRD causes the operator to converge worker replicas automatically (no manual CLI), observable via metrics.
- A revoked/expired marketplace entitlement disables new warehouse activation within the refresh window, with an audit trail.
- Audit events land in the object-locked bucket continuously; chain verification passes against exported data.
- We can stand up a new customer's managed single-tenant stack from a runbook + pipeline in < 1 day with no manual scaling.

### Phase 3 — "Marketplace + compliance + multi-tenant"
**Theme:** public marketplace listings, SOC2-style controls, and (optionally)
shared-cluster pooling.

**Workstreams & owners-by-crate**
- **SCIM provisioning hardening** *(opensnow-server/auth)* — exercise the existing SCIM surface (`auth.rs:3503`) against Okta/Entra/Google, add filtering/pagination conformance and de-provisioning lifecycle tests. **M**
- **Flight data-path + fault tolerance** *(opensnow-distributed)* — Gap #6: `do_get`/`do_put`, per-fragment retry + worker-loss reassignment. **L**
- **Multi-tenant pooling + metering/billing** *(operator, opensnow-server/tenant, new metering pipeline)* — Gap #7: per-tenant namespaces/quotas, noisy-neighbor controls, usage metering → billing (and AWS/GCP marketplace metering callbacks). **L**
- **SOC2-style controls** *(cross-crate + deploy)* — formal audit-export retention/object-lock policy, key-management runbook, access reviews driven by SCIM, change-management evidence, DR drill. **L**

**Exit criteria**
- Public AWS + GCP marketplace listings transact, with metering reported back and entitlement enforced.
- SCIM provisioning verified against ≥2 IdPs; de-provisioned users lose access promptly.
- Distributed queries survive a worker kill mid-flight (fault-tolerance test).
- For pooled SaaS: two tenants on one cluster cannot see or starve each other; per-tenant usage produces an accurate invoice.
- An auditor-ready control set maps to the implemented features.

---

## 5. What we can sell TODAY vs in one quarter

**Today (with a pilot/contract wrapper):**
- **Single-tenant BYOC installs on AWS or GCP**, operated *with* the customer.
  The data plane, infra-as-code, JWT/OIDC API auth, SCIM surface, and a
  tamper-evident audit log are real. Caveats to disclose: autoscaling across
  *multiple* warehouses is manual (CLI `operator-apply`) until the operator
  daemon ships; audit logs are queryable but not yet auto-exported to the
  object-locked bucket; marketplace entitlement metering is not live; large
  distributed queries fall back to local execution (no Flight result streaming
  yet); GCP Terraform lacks AWS's audit/KMS parity.

**In ~1 quarter (Phase 1 + start of Phase 2):**
- **BYOC GA** with validated AWS *and* GCP installs, automated secret-sync, and
  runtime entitlement enforcement; plus an early **managed single-tenant SaaS**
  once the operator daemon and audit export land.

**~2 quarters:** managed SaaS MVP fully baked (Phase 2 done), and the start of
marketplace + multi-tenant pooling (Phase 3). True **multi-tenant pooled SaaS
with usage-based billing** is the long pole and should not be promised before
Phase 3 completes.

---

## Appendix — verification index (file:line)

- Reconcile logic: `crates/opensnow-distributed/src/operator.rs:152`
- Shell apply loop (uncalled): `crates/opensnow-distributed/src/k8s.rs:181`
- Manual operator CLI: `crates/opensnow-cli/src/main.rs:1247`, `:1283`
- CRD: `deploy/crds/warehouses.opensnow.io.yaml`
- JWT validate / enterprise / JWKS: `crates/opensnow-auth/src/jwt.rs:424`, `:219`, `:461`
- JWT enforced (pgwire / HTTP): `crates/opensnow-server/src/pg.rs:233`; `crates/opensnow-server/src/auth.rs:1706`, `:1720`
- OIDC exchange: `crates/opensnow-auth/src/sso.rs:1081`, `:1010`, `:1141`
- OIDC admin fenced-off: `crates/opensnow-server/src/admin.rs:313`
- SCIM router: `crates/opensnow-server/src/auth.rs:3503`
- Audit chain: `crates/opensnow-catalog/src/lib.rs:1774`, `:1814`, `:550`
- Audit-export configure (no S3 ship): `crates/opensnow-server/src/auth.rs:2477`
- Entitlement gate (tested, not runtime-wired): `crates/opensnow-catalog/src/lib.rs:982`–`:1001`; `crates/opensnow-auth/src/contract.rs:527`
- Flight unimplemented: `crates/opensnow-distributed/src/coordinator.rs:172`, `:179`, `:190`
- Executor fallback / no retry: `crates/opensnow-distributed/src/distributed_executor.rs:97`, `:17`
- Tenant middleware: `crates/opensnow-server/src/tenant.rs:46`
- Terraform AWS audit bucket / object lock: `deploy/terraform/main.tf:239`, `:242`, `:252`
- Terraform AWS SSM hints (marketplace/entitlement/audit/rds): `deploy/terraform/main.tf:335`, `:346`, `:349`–`:351`
- Helm secret-provider fail-gate: `deploy/helm/opensnow/templates/configmap.yaml:51`
- Helm KEDA autoscaling: `deploy/helm/opensnow/templates/keda.yaml`
