#!/usr/bin/env python3
"""Provision the OpenSnow demo Metabase end-to-end via its REST API.

Idempotent-ish: expects a freshly reset Metabase (setup-token present). Creates
the admin user, connects the Postgres serving layer, builds one question per
mart, assembles a dashboard, and enables public sharing. Prints the public
dashboard URL (UUID) at the end so it can be wired into OPENSNOW_DASHBOARD_URL.

Env:
  MB_URL       base url (default http://localhost:3000)
  MB_EMAIL     admin email
  MB_PASSWORD  admin password
  PG_PASSWORD  Postgres (eurostat) password — the serving DB Metabase reads
"""
import json
import os
import sys
import time
import urllib.error
import urllib.request

MB = os.environ.get("MB_URL", "http://localhost:3000").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]
PASSWORD = os.environ["MB_PASSWORD"]
PG_PASSWORD = os.environ["PG_PASSWORD"]

SESSION = None


def api(method, path, body=None, headers=None):
    url = f"{MB}{path}"
    data = json.dumps(body).encode() if body is not None else None
    h = {"Content-Type": "application/json"}
    if SESSION:
        h["X-Metabase-Session"] = SESSION
    if headers:
        h.update(headers)
    req = urllib.request.Request(url, data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read().decode()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        print(f"  ! {method} {path} -> {e.code}: {e.read().decode()[:300]}", file=sys.stderr)
        raise


def wait_up():
    for _ in range(60):
        try:
            with urllib.request.urlopen(f"{MB}/api/health", timeout=10) as r:
                if r.status == 200:
                    return
        except Exception:
            pass
        time.sleep(3)
    sys.exit("Metabase never became healthy")


def main():
    global SESSION
    wait_up()
    props = api("GET", "/api/session/properties")
    token = props.get("setup-token")
    if not token:
        sys.exit("No setup-token — Metabase is not freshly reset. Reset its volume first.")

    print("==> creating admin user")
    # Note: passing a `database` block to /api/setup is silently dropped by recent
    # Metabase versions — add it via POST /api/database afterwards instead.
    setup = api("POST", "/api/setup", {
        "token": token,
        "user": {
            "first_name": "OpenSnow", "last_name": "Demo",
            "email": EMAIL, "password": PASSWORD, "site_name": "OpenSnow Demo",
        },
        "prefs": {"site_name": "OpenSnow Demo", "allow_tracking": False},
    })
    SESSION = setup if isinstance(setup, str) else setup.get("id")
    if not SESSION:
        SESSION = api("POST", "/api/session", {"username": EMAIL, "password": PASSWORD})["id"]

    print("==> enabling public sharing")
    api("PUT", "/api/setting/enable-public-sharing", {"value": True})

    print("==> connecting Postgres serving layer")
    db = api("POST", "/api/database", {
        "engine": "postgres", "name": "Eurostat (serving)",
        "details": {
            "host": "postgres", "port": 5432, "dbname": "eurostat",
            "user": "eurostat", "password": PG_PASSWORD, "ssl": False,
            "schema-filters-type": "all",
        },
    })
    db_id = db["id"]
    print(f"==> Postgres db id = {db_id}; syncing schema")
    api("POST", f"/api/database/{db_id}/sync_schema")

    # wait for tables to appear
    tables = {}
    for _ in range(40):
        meta = api("GET", f"/api/database/{db_id}/metadata")
        tables = {t["name"]: t for t in meta.get("tables", [])}
        if any(n.startswith("mart_") for n in tables):
            break
        time.sleep(3)
    print("   tables:", ", ".join(sorted(tables)))

    # one native (SQL) question per mart — robust to schema details
    CARDS = [
        ("Latest House Price Index (2015=100)",
         "SELECT geo, hpi_2015_100 FROM eurostat.mart_house_price_latest ORDER BY hpi_2015_100 DESC",
         {"display": "bar",
          "visualization_settings": {"graph.dimensions": ["geo"], "graph.metrics": ["hpi_2015_100"]}}),
        ("House Price Index over time",
         "SELECT geo, (year || '-Q' || quarter) AS period, hpi_2015_100 "
         "FROM eurostat.mart_house_price_index WHERE geo IN ('SE','DE','FR','ES','NL') ORDER BY year, quarter",
         {"display": "line",
          "visualization_settings": {"graph.dimensions": ["period", "geo"], "graph.metrics": ["hpi_2015_100"]}}),
        ("House Price YoY %",
         "SELECT geo, (year || '-Q' || quarter) AS period, yoy_pct "
         "FROM eurostat.mart_house_price_yoy WHERE geo IN ('SE','DE','FR','ES','NL') ORDER BY year, quarter",
         {"display": "line",
          "visualization_settings": {"graph.dimensions": ["period", "geo"], "graph.metrics": ["yoy_pct"]}}),
        ("GDP Growth QoQ %",
         "SELECT geo, (year || '-Q' || quarter) AS period, gdp_qoq_pct "
         "FROM eurostat.mart_gdp_growth_qoq WHERE geo IN ('SE','DE','FR','ES','NL','EU27_2020') ORDER BY year, quarter",
         {"display": "line",
          "visualization_settings": {"graph.dimensions": ["period", "geo"], "graph.metrics": ["gdp_qoq_pct"]}}),
    ]

    print("==> creating cards")
    card_ids = []
    for name, sql, extra in CARDS:
        card = api("POST", "/api/card", {
            "name": name,
            "dataset_query": {"type": "native", "native": {"query": sql}, "database": db_id},
            "display": extra["display"],
            "visualization_settings": extra.get("visualization_settings", {}),
        })
        card_ids.append(card["id"])
        print(f"   - {name} (card {card['id']})")

    print("==> creating dashboard")
    dash = api("POST", "/api/dashboard", {
        "name": "Eurostat — EU Housing & Growth",
        "description": "Built by OpenSnow: dbt models over Eurostat, served via Postgres.",
    })
    dash_id = dash["id"]

    # lay out 2x2
    positions = [(0, 0), (12, 0), (0, 8), (12, 8)]
    cards_payload = []
    for cid, (col, row) in zip(card_ids, positions):
        cards_payload.append({
            "id": -(len(cards_payload) + 1), "card_id": cid,
            "col": col, "row": row, "size_x": 12, "size_y": 8,
        })
    api("PUT", f"/api/dashboard/{dash_id}/cards", {"cards": cards_payload})

    print("==> enabling public link on dashboard")
    pub = api("POST", f"/api/dashboard/{dash_id}/public_link")
    uuid = pub["uuid"]
    public_url = f"{MB}/public/dashboard/{uuid}"
    print("\n=== DONE ===")
    print("PUBLIC_DASHBOARD_URL=" + public_url)


if __name__ == "__main__":
    main()
