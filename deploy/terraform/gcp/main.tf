###############################################################################
# OpenSnow on GCP — GKE Autopilot cluster + GCS warehouse bucket
#
# Autopilot manages nodes for you; we only declare the cluster, the bucket,
# and the IAM glue (a Google service account + Workload Identity binding)
# that lets OpenSnow pods write to GCS without a key file.
###############################################################################

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    google = {
      source  = "hashicorp/google"
      version = "~> 5.30"
    }
  }
}

provider "google" {
  project = var.project_id
  region  = var.region
}

variable "project_id" {
  description = "GCP project the resources live in."
  type        = string
}

variable "cluster_name" {
  description = "GKE cluster name."
  type        = string
  default     = "opensnow"
}

variable "region" {
  description = "GCP region for cluster and bucket."
  type        = string
  default     = "us-central1"
}

variable "warehouse_bucket" {
  description = "GCS bucket name for the OpenSnow warehouse (must be globally unique)."
  type        = string
  default     = "opensnow-warehouse"
}

variable "namespace" {
  description = "Kubernetes namespace OpenSnow runs in. Used for the Workload Identity binding."
  type        = string
  default     = "opensnow"
}

variable "service_account" {
  description = "Name of the OpenSnow pod ServiceAccount Workload Identity trusts."
  type        = string
  default     = "opensnow"
}

# ── GKE Autopilot ────────────────────────────────────────────────────────────

resource "google_container_cluster" "opensnow" {
  name             = var.cluster_name
  location         = var.region
  enable_autopilot = true

  # Autopilot requires release_channel; REGULAR keeps us a few weeks behind
  # the latest GKE minor — sane default for production.
  release_channel {
    channel = "REGULAR"
  }

  # Autopilot enables Workload Identity automatically — declared here for
  # documentation purposes.
  workload_identity_config {
    workload_pool = "${var.project_id}.svc.id.goog"
  }

  deletion_protection = false
}

# ── GCS warehouse bucket ─────────────────────────────────────────────────────

resource "google_storage_bucket" "warehouse" {
  name                        = var.warehouse_bucket
  location                    = var.region
  force_destroy               = false
  uniform_bucket_level_access = true

  versioning {
    enabled = true
  }
}

# ── Workload Identity for OpenSnow pods ──────────────────────────────────────

resource "google_service_account" "opensnow" {
  account_id   = "${var.cluster_name}-opensnow"
  display_name = "OpenSnow workload identity"
}

# Read/write only the warehouse bucket — least privilege.
resource "google_storage_bucket_iam_member" "opensnow_object_admin" {
  bucket = google_storage_bucket.warehouse.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.opensnow.email}"
}

# Bind the Kubernetes ServiceAccount (namespace/service_account) to this
# Google service account. The OpenSnow Helm chart should annotate the KSA
# with `iam.gke.io/gcp-service-account = google_service_account.opensnow.email`.
resource "google_service_account_iam_member" "opensnow_workload_identity" {
  service_account_id = google_service_account.opensnow.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${var.project_id}.svc.id.goog[${var.namespace}/${var.service_account}]"
}

# ── Outputs ──────────────────────────────────────────────────────────────────

output "project_id" {
  value       = var.project_id
  description = "GCP project id (echoed back for shell glue)."
}

output "cluster_name" {
  value       = google_container_cluster.opensnow.name
  description = "GKE cluster name."
}

output "region" {
  value       = var.region
  description = "Cluster region."
}

output "cluster_endpoint" {
  value       = google_container_cluster.opensnow.endpoint
  description = "GKE control plane endpoint."
}

output "kubeconfig_command" {
  value       = "gcloud container clusters get-credentials ${google_container_cluster.opensnow.name} --region ${var.region} --project ${var.project_id}"
  description = "Command to fetch the cluster's kubeconfig credentials."
}

output "warehouse_bucket" {
  value       = google_storage_bucket.warehouse.name
  description = "GCS bucket OpenSnow stores Parquet files in."
}

output "workload_identity_email" {
  value       = google_service_account.opensnow.email
  description = "Annotate the OpenSnow pod ServiceAccount with iam.gke.io/gcp-service-account = this email."
}
