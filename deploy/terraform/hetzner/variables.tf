variable "hcloud_token" {
  type        = string
  sensitive   = true
  description = "Hetzner Cloud API token (Project → Security → API Tokens, Read & Write). Can also be set via HCLOUD_TOKEN env."
  default     = null
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key for the root user (contents of ~/.ssh/id_*.pub)."
}

variable "server_type" {
  type        = string
  default     = "cx32" # 4 vCPU / 8 GB Intel — comfortably runs the stack
  description = "Hetzner server type (cx32, cpx41, cax21, …)."
}

variable "location" {
  type        = string
  default     = "hel1" # Helsinki (EU, closest to Sweden). Also: nbg1, fsn1.
  description = "Hetzner datacenter location."
}

variable "demo_domain" {
  type        = string
  default     = "opensnow.example.com"
  description = "Public domain for the OpenSnow console + pipeline."
}

variable "dash_domain" {
  type        = string
  default     = "metabase.example.com"
  description = "Public domain for the embedded Metabase dashboard."
}

variable "repo_url" {
  type        = string
  default     = "https://github.com/opensnow/opensnow.git"
  description = "Repo cloned on the VM to build/run the demo bundle."
}
