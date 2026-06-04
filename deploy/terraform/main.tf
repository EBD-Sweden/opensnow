###############################################################################
# OpenSnow on AWS — single-region EKS cluster + S3 warehouse bucket
#
# Provisions a small EKS control plane, one managed node group, an S3 bucket
# OpenSnow uses as its warehouse, and the IAM role / OIDC trust policy needed
# for IRSA so the OpenSnow pods can talk to S3 without long-lived credentials.
###############################################################################

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.40"
    }
    tls = {
      source  = "hashicorp/tls"
      version = "~> 4.0"
    }
  }
}

provider "aws" {
  region = var.region
}

data "aws_availability_zones" "available" {
  state = "available"
}

# ── Networking ───────────────────────────────────────────────────────────────
#
# Pulled from the AWS-maintained terraform-aws-modules/vpc to avoid
# hand-rolling the VPC, NAT gateway, and route tables.

module "vpc" {
  source  = "terraform-aws-modules/vpc/aws"
  version = "~> 5.5"

  name = "${var.cluster_name}-vpc"
  cidr = "10.0.0.0/16"

  azs             = slice(data.aws_availability_zones.available.names, 0, 3)
  private_subnets = ["10.0.1.0/24", "10.0.2.0/24", "10.0.3.0/24"]
  public_subnets  = ["10.0.101.0/24", "10.0.102.0/24", "10.0.103.0/24"]

  enable_nat_gateway   = true
  single_nat_gateway   = true
  enable_dns_hostnames = true
  enable_dns_support   = true

  # EKS expects these tags so the cluster's autoscaler / load balancer can
  # discover the right subnets.
  public_subnet_tags = {
    "kubernetes.io/role/elb"                    = 1
    "kubernetes.io/cluster/${var.cluster_name}" = "shared"
  }
  private_subnet_tags = {
    "kubernetes.io/role/internal-elb"           = 1
    "kubernetes.io/cluster/${var.cluster_name}" = "shared"
  }
}

# ── EKS cluster ──────────────────────────────────────────────────────────────

module "eks" {
  source  = "terraform-aws-modules/eks/aws"
  version = "~> 20.8"

  cluster_name    = var.cluster_name
  cluster_version = var.kubernetes_version

  vpc_id     = module.vpc.vpc_id
  subnet_ids = module.vpc.private_subnets

  cluster_endpoint_public_access = true
  enable_irsa                    = true

  eks_managed_node_groups = {
    default = {
      desired_size = var.node_count
      min_size     = var.node_count
      max_size     = var.node_count + 2

      instance_types = [var.instance_type]
      capacity_type  = "ON_DEMAND"
    }
  }
}

# ── S3 warehouse bucket ──────────────────────────────────────────────────────

resource "aws_s3_bucket" "warehouse" {
  bucket        = var.warehouse_bucket
  force_destroy = false
}

resource "aws_s3_bucket_versioning" "warehouse" {
  bucket = aws_s3_bucket.warehouse.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_kms_key" "opensnow" {
  count                   = var.kms_key_arn == "" ? 1 : 0
  description             = "OpenSnow BYOC warehouse, metadata, audit, and sealed-secret encryption"
  deletion_window_in_days = 30
  enable_key_rotation     = true
}

locals {
  opensnow_kms_key_arn    = var.kms_key_arn != "" ? var.kms_key_arn : aws_kms_key.opensnow[0].arn
  audit_export_bucket     = var.audit_export_bucket != "" ? var.audit_export_bucket : "${var.cluster_name}-audit"
  marketplace_enabled     = var.deployment_mode == "aws-marketplace"
  marketplace_entitlement = var.marketplace_entitlement.entitlement_id
  rds_secret_arn          = var.rds_secret_arn != "" ? var.rds_secret_arn : (var.create_rds ? aws_db_instance.opensnow_metadata[0].master_user_secret[0].secret_arn : "")
}

resource "aws_s3_bucket_server_side_encryption_configuration" "warehouse" {
  bucket = aws_s3_bucket.warehouse.id
  rule {
    apply_server_side_encryption_by_default {
      kms_master_key_id = local.opensnow_kms_key_arn
      sse_algorithm     = "aws:kms"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "warehouse" {
  bucket                  = aws_s3_bucket.warehouse.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# ── IAM role for IRSA — pod-level S3 access ─────────────────────────────────
#
# The OpenSnow Helm chart should annotate the pod ServiceAccount with
# `eks.amazonaws.com/role-arn = aws_iam_role.opensnow_irsa.arn`. EKS' OIDC
# provider then issues short-lived credentials scoped to this role.

data "aws_iam_policy_document" "opensnow_irsa_assume" {
  statement {
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [module.eks.oidc_provider_arn]
    }

    condition {
      test     = "StringEquals"
      variable = "${module.eks.oidc_provider}:sub"
      values   = ["system:serviceaccount:${var.namespace}:${var.service_account}"]
    }
    condition {
      test     = "StringEquals"
      variable = "${module.eks.oidc_provider}:aud"
      values   = ["sts.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "opensnow_irsa" {
  name               = "${var.cluster_name}-opensnow-irsa"
  assume_role_policy = data.aws_iam_policy_document.opensnow_irsa_assume.json
}

data "aws_iam_policy_document" "opensnow_s3" {
  statement {
    sid    = "WarehouseBucketList"
    effect = "Allow"
    actions = [
      "s3:ListBucket",
      "s3:GetBucketLocation",
    ]
    resources = [aws_s3_bucket.warehouse.arn]
  }

  statement {
    sid    = "WarehouseObjectReadWrite"
    effect = "Allow"
    actions = [
      "s3:GetObject",
      "s3:PutObject",
      "s3:DeleteObject",
    ]
    resources = ["${aws_s3_bucket.warehouse.arn}/*"]
  }

  statement {
    sid    = "AuditExportWriteOnly"
    effect = "Allow"
    actions = [
      "s3:ListBucket",
      "s3:GetBucketLocation",
    ]
    resources = [aws_s3_bucket.audit_export.arn]
  }

  statement {
    sid    = "AuditExportAppend"
    effect = "Allow"
    actions = [
      "s3:PutObject",
    ]
    resources = ["${aws_s3_bucket.audit_export.arn}/*"]
  }

  statement {
    sid    = "OpenSnowKmsUse"
    effect = "Allow"
    actions = [
      "kms:Decrypt",
      "kms:DescribeKey",
      "kms:Encrypt",
      "kms:GenerateDataKey",
    ]
    resources = [local.opensnow_kms_key_arn]
  }
}

resource "aws_iam_policy" "opensnow_s3" {
  name        = "${var.cluster_name}-opensnow-s3"
  description = "OpenSnow read/write access to the warehouse bucket"
  policy      = data.aws_iam_policy_document.opensnow_s3.json
}

resource "aws_iam_role_policy_attachment" "opensnow_s3" {
  role       = aws_iam_role.opensnow_irsa.name
  policy_arn = aws_iam_policy.opensnow_s3.arn
}

# ── Enterprise BYOC optional metadata and audit resources ─────────────────────

resource "aws_s3_bucket" "audit_export" {
  bucket              = local.audit_export_bucket
  force_destroy       = false
  object_lock_enabled = true
}

resource "aws_s3_bucket_versioning" "audit_export" {
  bucket = aws_s3_bucket.audit_export.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_object_lock_configuration" "audit_export" {
  bucket = aws_s3_bucket.audit_export.id

  rule {
    default_retention {
      mode = "GOVERNANCE"
      days = 365
    }
  }
}

resource "aws_s3_bucket_public_access_block" "audit_export" {
  bucket                  = aws_s3_bucket.audit_export.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "audit_export" {
  bucket = aws_s3_bucket.audit_export.id
  rule {
    apply_server_side_encryption_by_default {
      kms_master_key_id = local.opensnow_kms_key_arn
      sse_algorithm     = "aws:kms"
    }
  }
}

resource "aws_db_subnet_group" "opensnow" {
  count      = var.create_rds ? 1 : 0
  name       = "${var.cluster_name}-metadata"
  subnet_ids = module.vpc.private_subnets
}

resource "aws_security_group" "rds" {
  count       = var.create_rds ? 1 : 0
  name        = "${var.cluster_name}-opensnow-rds"
  description = "Private RDS PostgreSQL access from OpenSnow EKS worker nodes only"
  vpc_id      = module.vpc.vpc_id

  egress {
    description = "Return traffic"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

resource "aws_vpc_security_group_ingress_rule" "rds_from_eks_nodes" {
  count                        = var.create_rds ? 1 : 0
  description                  = "PostgreSQL from EKS node security group"
  security_group_id            = aws_security_group.rds[0].id
  referenced_security_group_id = module.eks.node_security_group_id
  from_port                    = 5432
  to_port                      = 5432
  ip_protocol                  = "tcp"
}

resource "aws_db_instance" "opensnow_metadata" {
  count                       = var.create_rds ? 1 : 0
  identifier                  = "${var.cluster_name}-metadata"
  engine                      = "postgres"
  engine_version              = "16"
  instance_class              = var.rds_instance_class
  allocated_storage           = 50
  storage_encrypted           = true
  kms_key_id                  = local.opensnow_kms_key_arn
  db_name                     = "opensnow"
  username                    = "opensnow"
  manage_master_user_password = true
  db_subnet_group_name        = aws_db_subnet_group.opensnow[0].name
  vpc_security_group_ids      = [aws_security_group.rds[0].id]
  publicly_accessible         = false
  backup_retention_period     = 7
  backup_window               = "03:00-04:00"
  maintenance_window          = "sun:04:00-sun:05:00"
  auto_minor_version_upgrade  = true
  skip_final_snapshot         = false
  deletion_protection         = true
}

resource "aws_ssm_parameter" "enterprise_helm_hints" {
  name = "/${var.cluster_name}/opensnow/enterprise-helm-hints"
  type = "String"
  value = jsonencode({
    deployment_mode         = var.deployment_mode
    oidc_issuer_url         = var.oidc_issuer_url
    jwt_issuer_url          = var.jwt_issuer_url
    jwt_audience            = var.jwt_audience
    jwt_key_secret_name     = var.jwt_key_secret_name
    jwt_active_kid          = var.jwt_active_kid
    scim_enabled            = var.scim_enabled
    audit_export_bucket     = aws_s3_bucket.audit_export.bucket
    kms_key_arn             = local.opensnow_kms_key_arn
    pgwire_exposure         = var.pgwire_exposure
    marketplace_enabled     = local.marketplace_enabled
    marketplace_entitlement = local.marketplace_entitlement
    rds_secret_arn          = local.rds_secret_arn
  })
}
