###############################################################################
# OpenSnow public demo on Hetzner Cloud (EU).
#
# Provisions one server + a firewall (22/80/443) and bootstraps the deploy/demo
# docker-compose stack via cloud-init. `terraform apply` → the box builds and
# runs the demo; point DNS at the output IP and do the one-time Metabase setup.
###############################################################################

terraform {
  required_version = ">= 1.5.0"
  required_providers {
    hcloud = {
      source  = "hetznercloud/hcloud"
      version = "~> 1.47"
    }
  }
}

provider "hcloud" {
  token = var.hcloud_token
}

resource "hcloud_ssh_key" "demo" {
  name       = "opensnow-demo"
  public_key = var.ssh_public_key
}

resource "hcloud_firewall" "demo" {
  name = "opensnow-demo-fw"

  dynamic "rule" {
    for_each = ["22", "80", "443"]
    content {
      direction  = "in"
      protocol   = "tcp"
      port       = rule.value
      source_ips = ["0.0.0.0/0", "::/0"]
    }
  }
}

resource "hcloud_server" "demo" {
  name         = "opensnow-demo"
  server_type  = var.server_type # cx32 = 4 vCPU / 8 GB
  image        = "ubuntu-22.04"
  location     = var.location # hel1 = Helsinki (closest EU to Sweden)
  ssh_keys     = [hcloud_ssh_key.demo.id]
  firewall_ids = [hcloud_firewall.demo.id]

  user_data = templatefile("${path.module}/cloud-init.yaml", {
    demo_domain = var.demo_domain
    dash_domain = var.dash_domain
    repo_url    = var.repo_url
  })

  public_net {
    ipv4_enabled = true
    ipv6_enabled = true
  }
}
