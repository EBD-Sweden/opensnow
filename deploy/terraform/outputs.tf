output "cluster_name" {
  description = "EKS cluster name."
  value       = module.eks.cluster_name
}

output "cluster_endpoint" {
  description = "EKS API server endpoint."
  value       = module.eks.cluster_endpoint
}

output "cluster_certificate_authority_data" {
  description = "Base64-encoded cluster CA bundle (use to configure kubectl)."
  value       = module.eks.cluster_certificate_authority_data
  sensitive   = true
}

output "kubeconfig_command" {
  description = "Command to update local kubeconfig with this cluster."
  value       = "aws eks update-kubeconfig --region ${var.region} --name ${var.cluster_name}"
}

output "warehouse_bucket" {
  description = "S3 bucket OpenSnow stores Parquet files in."
  value       = aws_s3_bucket.warehouse.bucket
}

output "irsa_role_arn" {
  description = "IAM role ARN to annotate the OpenSnow pod ServiceAccount with."
  value       = aws_iam_role.opensnow_irsa.arn
}

output "kms_key_arn" {
  description = "KMS key ARN for OpenSnow BYOC encryption."
  value       = local.opensnow_kms_key_arn
}

output "audit_export_bucket" {
  description = "Customer-owned S3 bucket for OpenSnow audit export."
  value       = aws_s3_bucket.audit_export.bucket
}

output "metadata_rds_endpoint" {
  description = "Optional customer-owned RDS metadata endpoint."
  value       = var.create_rds ? aws_db_instance.opensnow_metadata[0].address : null
}

output "metadata_rds_master_secret_arn" {
  description = "Secrets Manager ARN for the managed or customer-supplied RDS master password; sync this to the Kubernetes Secret referenced by metadata.external.existingSecret."
  value       = local.rds_secret_arn != "" ? local.rds_secret_arn : null
  sensitive   = true
}

output "enterprise_helm_hints_parameter" {
  description = "SSM parameter containing Helm values hints for enterprise/BYOC install."
  value       = aws_ssm_parameter.enterprise_helm_hints.name
}
