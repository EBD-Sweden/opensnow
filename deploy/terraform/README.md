# OpenSnow on AWS / GCP — Terraform

Two ready-to-apply modules that stand up the cluster and warehouse bucket
OpenSnow needs, then point you at the Helm chart in `deploy/helm/opensnow`.

```
deploy/terraform/
├── main.tf       # AWS: EKS + S3 + IRSA role
├── variables.tf
├── outputs.tf
└── gcp/
    └── main.tf   # GCP: GKE Autopilot + GCS + Workload Identity
```

## AWS quickstart

```bash
cd deploy/terraform

terraform init
terraform apply -var="warehouse_bucket=my-org-opensnow-warehouse"

# Wire up kubectl and install the Helm chart.
$(terraform output -raw kubeconfig_command)
helm install opensnow ../helm/opensnow \
  --namespace opensnow --create-namespace \
  --set serviceAccount.annotations."eks\.amazonaws\.com/role-arn"="$(terraform output -raw irsa_role_arn)" \
  --set storage.warehouseBucket="$(terraform output -raw warehouse_bucket)"
```

`.terraform.lock.hcl is intentionally tracked` for the AWS root module so provider selections are reproducible across marketplace/BYOC validation runs. Do not commit deploy/terraform/.terraform/; it contains generated provider and module downloads and is ignored by the repository.

The defaults provision a private-node EKS cluster, a `t3.xlarge` node group of three nodes, a versioned/SSE-KMS S3 warehouse bucket, and a versioned Object-Lock audit bucket. IRSA trust is scoped to the `opensnow/opensnow` ServiceAccount; change `var.namespace` / `var.service_account` if you install the chart somewhere else. No static AWS access keys are rendered into Helm: pods use the IRSA annotation, and RDS credentials stay in AWS Secrets Manager until your external-secret controller syncs them into the Kubernetes Secret named by `metadata.external.existingSecret`.

## AWS BYOC / Marketplace enterprise install

Run this module in the customer-owned AWS account. OpenSnow stores warehouse
data in the customer's S3 bucket, metadata in customer RDS when `create_rds=true`,
audit logs in the customer audit bucket, and secrets behind customer KMS/Secrets
Manager. AWS Marketplace mode supplies marketplace entitlement identity to the
Helm chart; it does not replace OpenSnow SQL/RBAC authorization.

```bash
cd deploy/terraform
terraform init
terraform validate
terraform apply \
  -var="deployment_mode=aws-marketplace" \
  -var="warehouse_bucket=acme-opensnow-warehouse" \
  -var="audit_export_bucket=acme-opensnow-audit" \
  -var="create_rds=true" \
  -var='marketplace_entitlement={product_code="prod-abc",customer_identifier="cust-123",entitlement_id="ent-789"}'

$(terraform output -raw kubeconfig_command)
helm upgrade --install opensnow ../helm/opensnow \
  --namespace opensnow --create-namespace \
  -f ../helm/opensnow/values-enterprise-aws.yaml \
  --set serviceAccount.annotations."eks\.amazonaws\.com/role-arn"="$(terraform output -raw irsa_role_arn)" \
  --set config.storage.bucket="$(terraform output -raw warehouse_bucket)" \
  --set enterprise.auditExport.bucket="$(terraform output -raw audit_export_bucket)"
```

For OpenSnow test-instance mode, keep `deployment_mode=test-instance`, use the
standard Helm values, and do not claim enterprise SSO/SCIM/marketplace readiness.

## GCP quickstart

```bash
cd deploy/terraform/gcp

terraform init
terraform apply \
  -var="project_id=my-gcp-project" \
  -var="warehouse_bucket=my-org-opensnow-warehouse"

# GKE Autopilot kubeconfig.
gcloud container clusters get-credentials \
  $(terraform output -raw cluster_name) \
  --region $(terraform output -raw region) \
  --project $(terraform output -raw project_id)

helm install opensnow ../../helm/opensnow \
  --namespace opensnow --create-namespace \
  --set serviceAccount.annotations."iam\.gke\.io/gcp-service-account"="$(terraform output -raw workload_identity_email)" \
  --set storage.warehouseBucket="$(terraform output -raw warehouse_bucket)"
```

Workload Identity is wired up for you; the GCP service account has read/write
on the warehouse bucket only.

## Variables (AWS)

| Name               | Default              | Description                                                      |
| ------------------ | -------------------- | ---------------------------------------------------------------- |
| `cluster_name`     | `opensnow`           | EKS cluster name; also prefixes IAM and bucket resources         |
| `region`           | `us-east-1`          | AWS region                                                       |
| `kubernetes_version` | `1.30`             | EKS control-plane version                                        |
| `node_count`       | `3`                  | Number of nodes in the managed node group                        |
| `instance_type`    | `t3.xlarge`          | EC2 instance type for nodes                                      |
| `warehouse_bucket` | `opensnow-warehouse` | Must be globally unique — pick something namespaced              |
| `namespace`        | `opensnow`           | K8s namespace IRSA trusts                                        |
| `service_account`  | `opensnow`           | Pod ServiceAccount IRSA trusts                                   |
| `deployment_mode`  | `test-instance`      | `test-instance`, `enterprise`, or `aws-marketplace`              |
| `create_rds`       | `false`              | Create customer-owned RDS PostgreSQL metadata store              |
| `rds_secret_arn`   | empty                | Customer Secrets Manager ARN for RDS password handoff            |
| `kms_key_arn`      | empty                | Existing customer-managed KMS key; empty creates one             |
| `oidc_issuer_url`  | empty                | Customer IdP issuer for Helm enterprise auth values              |
| `jwt_issuer_url`   | empty                | OpenSnow product-token issuer for Helm `enterprise.jwt.issuer`   |
| `jwt_audience`     | `opensnow-api`       | Expected audience for RS256/ES256 product tokens                 |
| `jwt_key_secret_name` | empty             | External/K8s Secret with JWT private/public PEM and JWK fields   |
| `jwt_active_kid`   | empty                | Active product-token signing key id for JWKS rotation            |
| `scim_enabled`     | `false`              | Requires SCIM lifecycle provisioning in enterprise values         |
| `audit_export_bucket` | empty             | Customer-owned audit export bucket; empty creates one             |
| `pgwire_exposure`  | `disabled`           | `disabled`, `private`, or `public` pgwire exposure plan          |

## Variables (GCP)

| Name               | Default              | Description                                                      |
| ------------------ | -------------------- | ---------------------------------------------------------------- |
| `project_id`       | (required)           | GCP project to deploy into                                       |
| `cluster_name`     | `opensnow`           | GKE cluster name                                                 |
| `region`           | `us-central1`        | GCP region for the cluster and bucket                            |
| `warehouse_bucket` | `opensnow-warehouse` | Must be globally unique                                          |
| `namespace`        | `opensnow`           | K8s namespace Workload Identity trusts                           |
| `service_account`  | `opensnow`           | Pod ServiceAccount Workload Identity trusts                      |

## Outputs

Each module exposes:

* `cluster_name`, `cluster_endpoint` — for kubectl / kubeconfig setup
* `kubeconfig_command` — copy-paste command that wires up `kubectl`
* `warehouse_bucket` — feed into the chart's `storage.warehouseBucket`
* `irsa_role_arn` (AWS) / `workload_identity_email` (GCP) — annotation for the pod ServiceAccount
* `kms_key_arn`, `audit_export_bucket`, `metadata_rds_endpoint`, `metadata_rds_master_secret_arn` — enterprise/BYOC Helm inputs and external-secret wiring

## Operations runbooks

### Upgrade and rollback

1. Save the currently deployed values: `helm get values opensnow -n opensnow -o yaml > /tmp/opensnow-values-before.yaml`.
2. Render before applying: `helm template opensnow ../helm/opensnow -f ../helm/opensnow/values-enterprise-aws.yaml > /tmp/opensnow-render.yaml` and confirm no inline cloud credentials or deterministic secrets are present.
3. Upgrade atomically: `helm upgrade --install opensnow ../helm/opensnow -n opensnow --atomic --timeout 10m -f ../helm/opensnow/values-enterprise-aws.yaml ...`.
4. Roll back on failed smoke or operator decision: `helm rollback opensnow <REVISION> -n opensnow`.

### Backup and restore

- Warehouse data: keep S3 versioning and SSE-KMS enabled; use AWS Backup or scheduled `aws s3 sync`/S3 replication according to the customer RPO.
- Metadata: when `create_rds=true`, RDS has 7-day automated backups and deletion protection. Restore with `aws rds restore-db-instance-to-point-in-time`, sync the restored secret to `metadata.external.existingSecret`, update `metadata.external.host`, then run `helm upgrade --install`.
- Audit export: the audit bucket is versioned and Object-Lock protected in Governance mode for append-only retention. Do not grant OpenSnow delete rights on audit objects.

### Uninstall and teardown

```bash
helm uninstall opensnow -n opensnow
terraform destroy
```

S3 deletion will fail while warehouse/audit objects remain and RDS final snapshots/deletion protection prevent accidental metadata loss. Empty buckets and explicitly disable RDS deletion protection only after a signed-off data-retention decision.
