# OCI API credentials. Generate with `oci setup config`, or create an API key in
# the console (User Settings → API Keys) and fill these in terraform.tfvars.
variable "tenancy_ocid" {
  type        = string
  description = "Tenancy OCID."
}

variable "user_ocid" {
  type        = string
  description = "User OCID for the API key."
}

variable "fingerprint" {
  type        = string
  description = "API key fingerprint."
}

variable "private_key_path" {
  type        = string
  description = "Path to the API private key (PEM)."
}

variable "region" {
  type        = string
  description = "OCI region, e.g. eu-stockholm-1 or eu-frankfurt-1."
}

variable "compartment_ocid" {
  type        = string
  description = "Compartment OCID the demo resources live in (often the tenancy OCID)."
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key for the `ubuntu` user (contents of ~/.ssh/id_*.pub)."
}

# Always-Free A1 budget is 4 OCPU / 24 GB total; 2/12 leaves headroom.
variable "ocpus" {
  type        = number
  default     = 2
  description = "ARM OCPUs (Always-Free total budget is 4)."
}

variable "memory_gbs" {
  type        = number
  default     = 12
  description = "Memory in GB (Always-Free total budget is 24)."
}

variable "demo_domain" {
  type        = string
  default     = "opensnow.ebdsweden.com"
  description = "Public domain for the OpenSnow console + pipeline."
}

variable "dash_domain" {
  type        = string
  default     = "metabase.ebdsweden.com"
  description = "Public domain for the embedded Metabase dashboard."
}

variable "repo_url" {
  type        = string
  default     = "https://github.com/EBD-Sweden/opensnow"
  description = "Repo cloned on the VM to build/run the demo bundle."
}
