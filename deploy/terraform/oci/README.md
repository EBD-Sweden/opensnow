# OpenSnow public demo on Oracle Cloud (Terraform, Always-Free)

`terraform apply` provisions a free-tier **ARM Ampere A1** VM + networking and
bootstraps the `deploy/demo` docker-compose stack via cloud-init — so
your configured OpenSnow demo domain comes up automatically.

## Prerequisites

- An Oracle Cloud account (Always-Free is enough).
- Terraform ≥ 1.5, or use the OCI CLI equivalent (`oci setup config` to create the
  API key this module needs).
- API key configured: tenancy/user OCID, fingerprint, private key, region.

## Use

```bash
cd deploy/terraform/oci
cp terraform.tfvars.example terraform.tfvars   # fill in your OCIDs + SSH key
terraform init
terraform apply
```

Outputs the VM's `public_ip`. Then:

1. **DNS:** point `opensnow.example.com` and `metabase.example.com` (A records)
   at that IP.
2. cloud-init installs Docker, builds OpenSnow (ARM build ~15–30 min on first
   boot), starts the stack, and runs the one-time seed. Watch it:
   ```bash
   ssh ubuntu@<public_ip> 'cloud-init status --wait && docker ps'
   ```
3. Once DNS resolves, Caddy obtains TLS automatically → `https://opensnow.example.com`.
4. Do the Metabase first-run + Public Sharing (see `../../demo/README.md`).

## Gotchas

- **Capacity:** `VM.Standard.A1.Flex` free capacity is frequently exhausted —
  `apply` may fail with *"Out of host capacity"*. Retry, or change `region` /
  availability domain. (This is an Oracle free-tier reality, not a config error.)
- **Budget:** Always-Free is 4 OCPU / 24 GB total across A1 instances; defaults
  here use 2 / 12.
- Want pure CLI instead of Terraform? The same resources map 1:1 to
  `oci network vcn create`, `oci network subnet create`, `oci compute instance
  launch …` — but Terraform handles the dependency graph and re-runs for you.

## Destroy

```bash
terraform destroy
```
