variable "cluster_name" {
  description = "EKS cluster name. Also used as a prefix for IAM and S3 resources."
  type        = string
  default     = "opensnow"
}

variable "region" {
  description = "AWS region to deploy into."
  type        = string
  default     = "us-east-1"
}

variable "kubernetes_version" {
  description = "EKS control-plane Kubernetes version."
  type        = string
  default     = "1.30"
}

variable "node_count" {
  description = "Desired number of nodes in the managed node group."
  type        = number
  default     = 3
}

variable "instance_type" {
  description = "EC2 instance type for the node group."
  type        = string
  default     = "t3.xlarge"
}

variable "warehouse_bucket" {
  description = "Globally unique S3 bucket name OpenSnow uses as its warehouse."
  type        = string
  default     = "opensnow-warehouse"
}

variable "namespace" {
  description = "Kubernetes namespace the OpenSnow Helm release runs in. Used to scope the IRSA trust policy."
  type        = string
  default     = "opensnow"
}

variable "service_account" {
  description = "Name of the OpenSnow pod ServiceAccount that the IRSA role trusts."
  type        = string
  default     = "opensnow"
}

variable "deployment_mode" {
  description = "OpenSnow install mode: local test-instance, customer enterprise BYOC, or AWS Marketplace entitlement-backed BYOC."
  type        = string
  default     = "test-instance"

  validation {
    condition     = contains(["test-instance", "enterprise", "aws-marketplace"], var.deployment_mode)
    error_message = "deployment_mode must be one of test-instance, enterprise, or aws-marketplace."
  }
}

variable "create_rds" {
  description = "Create customer-owned RDS PostgreSQL metadata store for enterprise/BYOC deployments."
  type        = bool
  default     = false
}

variable "rds_instance_class" {
  description = "RDS PostgreSQL instance class when create_rds is true."
  type        = string
  default     = "db.t4g.medium"
}

variable "rds_secret_arn" {
  description = "AWS Secrets Manager secret ARN holding the RDS password; required by Helm metadata.external.existingSecret sync path."
  type        = string
  default     = ""
}

variable "kms_key_arn" {
  description = "Existing customer-managed KMS key ARN for warehouse, RDS, audit export, and sealed-secret encryption. Empty creates a dedicated key."
  type        = string
  default     = ""
}

variable "oidc_issuer_url" {
  description = "Customer-owned IdP OIDC issuer URL to pass into Helm enterprise.oidc.issuer."
  type        = string
  default     = ""
}

variable "scim_enabled" {
  description = "Whether enterprise SCIM lifecycle provisioning is required for the Helm release."
  type        = bool
  default     = false
}

variable "jwt_issuer_url" {
  description = "OpenSnow product-token issuer URL to pass into Helm enterprise.jwt.issuer for RS256/ES256 enterprise mode."
  type        = string
  default     = ""
}

variable "jwt_audience" {
  description = "OpenSnow product-token audience to pass into Helm enterprise.jwt.audience."
  type        = string
  default     = "opensnow-api"
}

variable "jwt_key_secret_name" {
  description = "Kubernetes/ExternalSecret name containing enterprise JWT private/public PEMs and public JWK coordinates."
  type        = string
  default     = ""
}

variable "jwt_active_kid" {
  description = "Active enterprise JWT signing key id (kid) used for Helm enterprise.jwt.kid and JWKS rotation."
  type        = string
  default     = ""
}

variable "audit_export_bucket" {
  description = "Customer-owned S3 bucket for append-only OpenSnow audit export. Empty creates a cluster-name-derived audit bucket."
  type        = string
  default     = ""
}

variable "pgwire_exposure" {
  description = "PostgreSQL wire exposure mode: disabled, private, or public. Public must be paired with TLS and restricted source ranges in Helm."
  type        = string
  default     = "disabled"

  validation {
    condition     = contains(["disabled", "private", "public"], var.pgwire_exposure)
    error_message = "pgwire_exposure must be disabled, private, or public."
  }
}

variable "marketplace_entitlement" {
  description = "AWS Marketplace entitlement identity for account/warehouse activation gating."
  type = object({
    product_code        = string
    customer_identifier = string
    entitlement_id      = string
  })
  default = {
    product_code        = ""
    customer_identifier = ""
    entitlement_id      = ""
  }
}
