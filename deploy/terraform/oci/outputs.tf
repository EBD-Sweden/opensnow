output "public_ip" {
  value       = oci_core_instance.demo.public_ip
  description = "Point opensnow.ebdsweden.com and metabase.ebdsweden.com (A records) at this IP."
}

output "ssh" {
  value       = "ssh ubuntu@${oci_core_instance.demo.public_ip}"
  description = "SSH into the VM (e.g. to watch the cloud-init bootstrap: cloud-init status --wait)."
}

output "next_steps" {
  value = join("\n", [
    "1. DNS: A records ${var.demo_domain} + ${var.dash_domain} -> ${oci_core_instance.demo.public_ip}",
    "2. Bootstrap runs via cloud-init (docker + compose up + seed). Watch: ssh ubuntu@${oci_core_instance.demo.public_ip} 'cloud-init status --wait && docker ps'",
    "3. Once DNS resolves, Caddy gets TLS automatically: https://${var.demo_domain}",
    "4. Metabase first-run + Public Sharing at https://${var.dash_domain} (see deploy/demo/README.md).",
  ])
}
