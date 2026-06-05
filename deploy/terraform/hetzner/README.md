# OpenSnow public demo on Hetzner Cloud (EU)

`terraform apply` provisions one Hetzner server + firewall and bootstraps the
`deploy/demo` docker-compose stack via cloud-init. Default `cx32` (4 vCPU / 8 GB,
~€7.6 / ~85 SEK/mo) in Helsinki — comfortably runs OpenSnow + Postgres + Metabase.

## Prerequisites

1. A Hetzner Cloud account → create a **Project**.
2. In the project: **Security → API Tokens → Generate** (Read & Write). Copy it.
3. Terraform ≥ 1.5 and an SSH key.

## Use

```bash
cd deploy/terraform/hetzner
export HCLOUD_TOKEN=<your-token>          # or put it in terraform.tfvars
cp terraform.tfvars.example terraform.tfvars   # set ssh_public_key (token optional if env set)
terraform init
terraform apply
```

Outputs the server's `public_ip`. Then:

1. **DNS:** A records `opensnow.ebdsweden.com` + `metabase.ebdsweden.com` → that IP.
2. cloud-init installs Docker, builds OpenSnow (x86 — ~5–10 min), starts the
   stack, runs the one-time seed. Watch: `ssh root@<ip> 'cloud-init status --wait && docker ps'`.
3. Caddy gets TLS automatically once DNS resolves → `https://opensnow.ebdsweden.com`.
4. Metabase first-run + Public Sharing (see `../../demo/README.md`).

## Sizes / cost

| `server_type` | vCPU / RAM | ~€/mo | ~SEK/mo |
|---|---|---|---|
| `cx32` (default) | 4 / 8 GB | 7.6 | ~85 |
| `cpx41` | 8 / 16 GB (AMD) | 16 | ~180 |
| `cax21` | 4 / 8 GB (Arm) | 6.5 | ~73 |

## Destroy

```bash
terraform destroy
```
