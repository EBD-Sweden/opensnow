# Auth mode. Default: reuse the OCI CLI's ~/.oci/config (simplest for local use).
variable "use_config_file" {
  type        = bool
  default     = true
  description = "If true, authenticate via ~/.oci/config (like the OCI CLI). If false, use the explicit API-key vars below (CI/non-interactive)."
}

variable "config_file_profile" {
  type        = string
  default     = "DEFAULT"
  description = "Profile name in ~/.oci/config (used when use_config_file=true)."
}

# Explicit OCI API credentials — only needed when use_config_file=false.
variable "tenancy_ocid" {
  type        = string
  default     = ""
  description = "Tenancy OCID (explicit-auth mode)."
}

variable "user_ocid" {
  type        = string
  default     = ""
  description = "User OCID for the API key (explicit-auth mode)."
}

variable "fingerprint" {
  type        = string
  default     = ""
  description = "API key fingerprint (explicit-auth mode)."
}

variable "private_key_path" {
  type        = string
  default     = ""
  description = "Path to the API private key PEM (explicit-auth mode)."
}

variable "private_key_password" {
  type        = string
  default     = ""
  sensitive   = true
  description = "Passphrase for the API private key, if it is encrypted (matches `pass_phrase` in ~/.oci/config)."
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

variable "image_ocid" {
  type        = string
  default     = ""
  description = "Ubuntu 22.04 aarch64 image OCID. Empty = auto-discover the latest for the region."
}

variable "availability_domain" {
  type        = string
  default     = ""
  description = "Availability domain name (e.g. mcDH:EU-STOCKHOLM-1-AD-1). Empty = use the first in the tenancy."
}
