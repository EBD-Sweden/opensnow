output "public_ip" {
  value       = hcloud_server.demo.ipv4_address
  description = "Point var.demo_domain + var.dash_domain (A records) at this IP."
}

output "ssh" {
  value       = "ssh root@${hcloud_server.demo.ipv4_address}"
  description = "SSH in (e.g. to watch the bootstrap: cloud-init status --wait; docker ps)."
}

output "next_steps" {
  value = join("\n", [
    "1. DNS: A records ${var.demo_domain} + ${var.dash_domain} -> ${hcloud_server.demo.ipv4_address}",
    "2. Bootstrap runs via cloud-init (docker + compose up + seed). Watch: ssh root@${hcloud_server.demo.ipv4_address} 'cloud-init status --wait && docker ps'",
    "3. Once DNS resolves, Caddy gets TLS automatically: https://${var.demo_domain}",
    "4. Metabase first-run + Public Sharing at https://${var.dash_domain} (see deploy/demo/README.md).",
  ])
}
