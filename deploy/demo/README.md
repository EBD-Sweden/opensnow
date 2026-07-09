# OpenSnow public demo — `opensnow.ebdsweden.com`

A one-box, ~$0 deployment for a public, testable OpenSnow demo, designed for an
**Oracle Cloud Always-Free** ARM VM. One `docker compose` stack:

```
opensnow.ebdsweden.com   →  Caddy (auto-HTTPS)  →  OpenSnow console + read-only /pipeline   (public, SQL-gated)
metabase.ebdsweden.com   →  Caddy (auto-HTTPS)  →  Metabase (dashboard, embedded into the EBD site)

internal-only (never published): OpenSnow pgwire + trusted SQL, Postgres serving layer, dbt seed
```

**Why this is safe to expose:** the public hits only the demo SQL gate
(`SELECT`/`CTAS`/`SHOW` only — no `COPY`/DDL), a 20s query timeout, server-side
pagination, and a read-only pipeline view. Admin/write endpoints and pgwire are
blocked at Caddy and not published. Metabase uses public (read-only) embedding.

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
opensnow.ebdsweden.com   A   <VM_PUBLIC_IP>
metabase.ebdsweden.com   A   <VM_PUBLIC_IP>
```

(Use any names you like; just set `OPENSNOW_DEMO_DOMAIN` / `OPENSNOW_DASH_DOMAIN`
to match. Caddy gets Let's Encrypt certs automatically once DNS resolves.)

## 3. Deploy

```bash
git clone https://github.com/EBD-Sweden/opensnow && cd opensnow/deploy/demo

export OPENSNOW_DEMO_DOMAIN=opensnow.ebdsweden.com
export OPENSNOW_DASH_DOMAIN=metabase.ebdsweden.com
export OPENSNOW_DEMO_PG_PASSWORD=$(openssl rand -hex 16)

docker compose up -d --build         # builds OpenSnow (ARM-native on the VM)
docker compose run --rm seed         # one-time: build marts + load Postgres
```

OpenSnow is now live at `https://opensnow.ebdsweden.com` (console + `/pipeline`).

## 4. Metabase first-run + public embedding (one time)

Fast path: let the included setup script create the Metabase admin user,
Postgres connection, cards, dashboard, and public sharing link:

```bash
export MB_URL=https://metabase.ebdsweden.com
export MB_EMAIL=admin@example.com
export MB_PASSWORD=$(openssl rand -hex 24)
export PG_PASSWORD="$OPENSNOW_DEMO_PG_PASSWORD"
python3 metabase-setup.py | tee metabase-setup.out

export OPENSNOW_DASHBOARD_URL=$(awk -F= '/^PUBLIC_DASHBOARD_URL=/{print $2}' metabase-setup.out)
docker compose up -d opensnow
```

Manual path: open `https://metabase.ebdsweden.com`:

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
   OPENSNOW_DASHBOARD_URL="https://metabase.ebdsweden.com/public/dashboard/<uuid>#bordered=false&titled=false" \
     docker compose up -d opensnow
   ```

## 5. Rebuild the canonical Krona dashboard

The current public "The Krona's Bargain" dashboard is reproduced by
`metabase-krona-v3.py`, not the older `metabase-build-krona.py` setup flow. v3 is
the canonical script for the live step-by-step dashboard linked from the
OpenSnow console:

```
https://metabase.ebdsweden.com/public/dashboard/00769301-ca5e-49b9-8626-8ce33dd01ea9
```

Script roles:

- `metabase-build-krona.py` — legacy v1 builder; creates a separate 7-card Krona
  dashboard and is kept for provenance.
- `metabase-krona-narrate.py` — legacy narrator; re-laid an existing Krona
  dashboard with text cards while reusing existing question cards.
- `metabase-krona-v2.py` — legacy Europe-wide league-table rebuild for the
  existing Krona public dashboard.
- `metabase-krona-v3.py` — recommended/current path; rebuilds the live Krona
  public dashboard as a focused Sweden/Denmark/Germany/France comparison,
  creates fresh question cards, enables per-card public links, and preserves the
  public dashboard UUID above.

Required env vars: `MB_EMAIL`, `MB_PASSWORD`; optional `MB_URL` defaults to
`https://metabase.ebdsweden.com`. Source secrets from the operator secrets file:

```bash
set -a; source /path/to/secrets.env; set +a
cd ./deploy/demo
export MB_URL=${MB_URL:-https://metabase.ebdsweden.com}
python3 metabase-krona-v3.py      # prints DASHBOARD .../00769301... + BLOG_UUIDS
```

Safety: the Krona scripts mutate the configured Metabase instance. They log in,
create cards, replace the dashboard layout, and enable public links. Run v3 only
against the live demo when you intend to update that public dashboard, or point
`MB_URL` at a disposable Metabase instance for rehearsal.

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
- `metabase-krona-v3.py` — canonical Krona dashboard rebuild script for the
  public UUID `00769301-ca5e-49b9-8626-8ce33dd01ea9`
- `dbt/` — the demo dbt project; `sample-data/` — bundled Eurostat Parquet
