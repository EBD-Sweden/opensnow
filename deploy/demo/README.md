# OpenSnow public demo — self-hosted template

A one-box deployment template for a public, testable OpenSnow demo. The default
example targets an **Oracle Cloud Always-Free** ARM VM, but the same compose stack
also runs on any Linux VM with Docker:

```
opensnow.example.com   →  Caddy (auto-HTTPS)  →  OpenSnow console + read-only /pipeline   (public, SQL-gated)
metabase.example.com   →  Caddy (auto-HTTPS)  →  Metabase (dashboard, optional embedding)

internal-only (never published): OpenSnow pgwire + trusted SQL, Postgres serving layer, dbt seed
```

This directory is OSS-safe: it contains generic templates only. Hosted sandbox
operators should keep their live domains, project ids, dashboard UUIDs, and
credential files outside git.

**Why this is safe to expose:** the public hits only the demo SQL gate
(`SELECT`/`CTAS`/`SHOW` only — no `COPY`/DDL), a 20s query timeout, server-side
pagination, and a read-only pipeline view. Admin/write endpoints and pgwire are
blocked at Caddy and not published. Metabase uses public (read-only) embedding
only if you explicitly configure it.

## 1. Provision the VM (Oracle Cloud Always-Free)

- Create an **Ampere A1 (ARM)** instance — Always-Free covers up to 4 OCPU / 24 GB.
  2 OCPU / 8–12 GB is plenty. Ubuntu 22.04/24.04.
- Open ports **80** and **443** in the instance's security list / NSG (and
  `iptables`/ufw if enabled).
- Install Docker + compose plugin:
  ```bash
  curl -fsSL https://get.docker.com | sh
  sudo usermod -aG docker $USER   # re-login
  ```

## 2. DNS

Point two records at the VM's public IP (A records):

```
opensnow.example.com   A   <VM_PUBLIC_IP>
metabase.example.com   A   <VM_PUBLIC_IP>
```

Use domains you control; set `OPENSNOW_DEMO_DOMAIN` / `OPENSNOW_DASH_DOMAIN` to
match. Caddy gets Let's Encrypt certs automatically once DNS resolves.

## 3. Deploy

```bash
git clone https://github.com/opensnow/opensnow.git && cd opensnow/deploy/demo

export OPENSNOW_DEMO_DOMAIN=opensnow.example.com
export OPENSNOW_DASH_DOMAIN=metabase.example.com
export OPENSNOW_DEMO_PG_PASSWORD=$(openssl rand -hex 16)

docker compose up -d --build         # builds OpenSnow (ARM-native on the VM)
docker compose run --rm seed         # one-time: build marts + load Postgres
```

OpenSnow is now live at `https://$OPENSNOW_DEMO_DOMAIN` (console + `/pipeline`).

## 4. Metabase first-run + public embedding (one time)

Fast path: let the included setup script create the Metabase admin user,
Postgres connection, cards, dashboard, and public sharing link:

```bash
export MB_URL=https://$OPENSNOW_DASH_DOMAIN
export MB_EMAIL=admin@example.com
export MB_PASSWORD=$(openssl rand -hex 24)
export PG_PASSWORD="$OPENSNOW_DEMO_PG_PASSWORD"
python3 metabase-setup.py | tee metabase-setup.out

export OPENSNOW_DASHBOARD_URL=$(awk -F= '/^PUBLIC_DASHBOARD_URL=/{print $2}' metabase-setup.out)
docker compose up -d opensnow
```

Manual path: open `https://$OPENSNOW_DASH_DOMAIN`:

1. Create the admin account.
2. Add a database → **PostgreSQL**: host `postgres`, port `5432`, db `eurostat`,
   user `eurostat`, password = your `OPENSNOW_DEMO_PG_PASSWORD`.
3. Build a dashboard over the `eurostat.mart_*` tables (or import one).
4. **Admin → Settings → Public Sharing → Enable.** Open the dashboard →
   Sharing → **Public link** → copy the `.../public/dashboard/<uuid>` URL.
5. Put that URL in `OPENSNOW_DASHBOARD_URL` so the OpenSnow Dashboards tab and
   "Open dashboard" button point to it:
   ```bash
   # redeploy opensnow with the dashboard link
   OPENSNOW_DASHBOARD_URL="https://$OPENSNOW_DASH_DOMAIN/public/dashboard/<uuid>#bordered=false&titled=false" \
     docker compose up -d opensnow
   ```

## 5. Rebuild a Krona dashboard from the sample marts

The Krona dashboard helper scripts are optional Metabase mutation tools. They log
in to the configured Metabase instance, create or update cards, and enable public
links. Run them only against a disposable Metabase instance for rehearsal or
against your hosted sandbox when you intentionally want to update that dashboard.

Script roles:

- `metabase-build-krona.py` — creates a new 7-card Krona dashboard and prints its
  public URL.
- `metabase-krona-narrate.py` — re-lays an existing Krona dashboard with text
  cards while reusing existing question cards.
- `metabase-krona-v2.py` — Europe-wide league-table rebuild for an existing
  Krona public dashboard.
- `metabase-krona-v3.py` — focused Sweden/Denmark/Germany/France comparison for
  an existing Krona public dashboard.

Required env vars: `MB_URL`, `MB_EMAIL`, `MB_PASSWORD`. Existing-dashboard
scripts also require `OPENSNOW_KRONA_PUBLIC_UUID`:

```bash
cd deploy/demo
export MB_URL=https://$OPENSNOW_DASH_DOMAIN
export MB_EMAIL=admin@example.com
export MB_PASSWORD=<metabase-admin-password>
export OPENSNOW_KRONA_PUBLIC_UUID=<metabase-public-dashboard-uuid>
python3 metabase-krona-v3.py
```

## Cost

Oracle Always-Free ARM = **$0/mo**. The only spend is the domain you already own.
A paid equivalent (e.g. Hetzner CX22 4 GB) is ~€4.50/mo if you prefer x86.

## Updating the demo data

Re-run the seed after changing models or sample data:
`docker compose run --rm seed`. To use the full Eurostat corpus, drop more
normalized Parquet into `sample-data/` and add them to `dbt/models/staging/sources.yml`.

## Files

- `docker-compose.yml` — the stack (OpenSnow, Postgres, Metabase, Caddy, seed)
- `Caddyfile` — TLS + routing + public endpoint allowlist
- `opensnow.demo.toml` — public-demo OpenSnow config
- `seed.Dockerfile` / `seed.sh` — one-time dbt build + Postgres export
- `metabase-setup.py` — optional one-time Metabase dashboard/public-link setup
- `metabase-krona-v3.py` — optional Krona dashboard rebuild script for an
  operator-provided public dashboard UUID
- `dbt/` — the demo dbt project; `sample-data/` — bundled Eurostat Parquet
