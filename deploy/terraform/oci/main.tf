###############################################################################
# OpenSnow public demo on Oracle Cloud Always-Free (ARM Ampere A1).
#
# Provisions a single free-tier ARM VM + minimal networking and bootstraps the
# `deploy/demo` docker-compose stack via cloud-init. `terraform apply` →
# opensnow.ebdsweden.com is live (after you point DNS at the output IP and do
# the one-time Metabase setup).
#
# Free-tier note: VM.Standard.A1.Flex capacity is often exhausted ("Out of host
# capacity"). If apply fails with that, retry, or try another availability
# domain / region.
###############################################################################

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    oci = {
      source  = "oracle/oci"
      version = "~> 5.0"
    }
  }
}

provider "oci" {
  tenancy_ocid     = var.tenancy_ocid
  user_ocid        = var.user_ocid
  fingerprint      = var.fingerprint
  private_key_path = var.private_key_path
  region           = var.region
}

# Pick the first availability domain in the compartment's tenancy.
data "oci_identity_availability_domains" "ads" {
  compartment_id = var.tenancy_ocid
}

# Latest Ubuntu 22.04 aarch64 image for the A1 shape (region-specific OCID resolved here).
data "oci_core_images" "ubuntu_arm" {
  compartment_id           = var.compartment_ocid
  operating_system         = "Canonical Ubuntu"
  operating_system_version = "22.04"
  shape                    = "VM.Standard.A1.Flex"
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

# ── Networking ────────────────────────────────────────────────────────────────
resource "oci_core_vcn" "demo" {
  compartment_id = var.compartment_ocid
  display_name   = "opensnow-demo-vcn"
  cidr_blocks    = ["10.0.0.0/16"]
}

resource "oci_core_internet_gateway" "demo" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.demo.id
  display_name   = "opensnow-demo-igw"
}

resource "oci_core_route_table" "demo" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.demo.id
  display_name   = "opensnow-demo-rt"
  route_rules {
    destination       = "0.0.0.0/0"
    network_entity_id = oci_core_internet_gateway.demo.id
  }
}

resource "oci_core_security_list" "demo" {
  compartment_id = var.compartment_ocid
  vcn_id         = oci_core_vcn.demo.id
  display_name   = "opensnow-demo-sl"

  egress_security_rules {
    destination = "0.0.0.0/0"
    protocol    = "all"
  }

  # SSH, HTTP, HTTPS from anywhere.
  dynamic "ingress_security_rules" {
    for_each = [22, 80, 443]
    content {
      protocol = "6" # TCP
      source   = "0.0.0.0/0"
      tcp_options {
        min = ingress_security_rules.value
        max = ingress_security_rules.value
      }
    }
  }
}

resource "oci_core_subnet" "demo" {
  compartment_id    = var.compartment_ocid
  vcn_id            = oci_core_vcn.demo.id
  cidr_block        = "10.0.1.0/24"
  display_name      = "opensnow-demo-subnet"
  route_table_id    = oci_core_route_table.demo.id
  security_list_ids = [oci_core_security_list.demo.id]
}

# ── Compute (Always-Free ARM) ─────────────────────────────────────────────────
resource "oci_core_instance" "demo" {
  compartment_id      = var.compartment_ocid
  availability_domain = data.oci_identity_availability_domains.ads.availability_domains[0].name
  display_name        = "opensnow-demo"
  shape               = "VM.Standard.A1.Flex"

  shape_config {
    ocpus         = var.ocpus
    memory_in_gbs = var.memory_gbs
  }

  create_vnic_details {
    subnet_id        = oci_core_subnet.demo.id
    assign_public_ip = true
  }

  source_details {
    source_type = "image"
    source_id   = data.oci_core_images.ubuntu_arm.images[0].id
  }

  metadata = {
    ssh_authorized_keys = var.ssh_public_key
    user_data = base64encode(templatefile("${path.module}/cloud-init.yaml", {
      demo_domain = var.demo_domain
      dash_domain = var.dash_domain
      repo_url    = var.repo_url
    }))
  }
}
