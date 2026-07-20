#!/usr/bin/env python3
"""Build the "Krona's Bargain" analytical-story dashboard in Metabase.

A weak currency is a wealth transfer: it hands exporters a competitiveness gift
that wage restraint never delivered, bills indebted households through the rate
hikes that follow, leaves a low-debt state insulated, and quietly rewards the
globally-diversified saver. This dashboard is the evidence behind the blog post.

Builds ONE dashboard (does not touch the existing seven), lays the cards in a
2-wide grid, enables a public link, and prints the public URL for embedding.

Env: MB_URL (default http://localhost:3000), MB_EMAIL, MB_PASSWORD

    MB_URL=http://localhost:3000 MB_EMAIL=admin@example.com MB_PASSWORD=*** \
        python3 metabase-build-krona.py
"""
import json
import os
import sys
import urllib.error
import urllib.request

MB = os.environ.get("MB_URL", "http://localhost:3000").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]
PASSWORD = os.environ["MB_PASSWORD"]
S = None

DASHBOARD_NAME = "The Krona's Bargain — a weak currency is a wealth transfer"
DASHBOARD_DESC = (
    "How Sweden's weak krona split winners from losers, 2010–2026. "
    "Built by OpenSnow over Eurostat/ECB/FMP — dbt marts served via Postgres."
)


def api(method, path, body=None):
    h = {"Content-Type": "application/json"}
    if S:
        h["X-Metabase-Session"] = S
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{MB}{path}", data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            raw = r.read().decode()
            return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        print(f"  ! {method} {path} -> {e.code}: {e.read().decode()[:300]}", file=sys.stderr)
        raise


def line(dims, metrics):
    return {"display": "line", "visualization_settings": {"graph.dimensions": dims, "graph.metrics": metrics}}


def bar(dims, metrics, stacked=False):
    vs = {"graph.dimensions": dims, "graph.metrics": metrics}
    if stacked:
        vs["stackable.stack_type"] = "stacked"
    return {"display": "bar", "visualization_settings": vs}


def table():
    return {"display": "table", "visualization_settings": {}}


# (title, sql, viz) — the narrative arc, two cards per act.
CARDS = [
    # ── Setup: the protagonist ──────────────────────────────────────────────
    ("The weak krona: it takes more SEK to buy a euro (year-end)",
     "SELECT year, sek_per_eur FROM eurostat.mart_sek_vs_euro "
     "WHERE sek_per_eur IS NOT NULL ORDER BY year",
     line(["year"], ["sek_per_eur"])),

    # ── Act 1: the firm wins — currency did the work, not wages ─────────────
    ("Act 1 — The payoff: Sweden's real exchange rate fell, Germany's couldn't "
     "(REER, 2015=100; lower = more competitive)",
     "SELECT year, geo, reer_cpi_index FROM eurostat.mart_competitiveness "
     "WHERE geo IN ('SE','DE') AND reer_cpi_index IS NOT NULL ORDER BY year, geo",
     line(["year", "geo"], ["reer_cpi_index"])),

    ("Act 1 — But no wage restraint: unit labour costs rose alike (2015=100)",
     "SELECT year, geo, ulc_index_2015 FROM eurostat.mart_competitiveness "
     "WHERE geo IN ('SE','DE') AND ulc_index_2015 IS NOT NULL ORDER BY year, geo",
     line(["year", "geo"], ["ulc_index_2015"])),

    # ── Act 2: the household pays — leverage + a lost half-decade ───────────
    ("Act 2 — Who pays: Sweden's households are heavily indebted, thin cash buffer "
     "(latest, EUR per capita; ratio = deposits ÷ debt)",
     "WITH l AS (SELECT max(year) y FROM eurostat.mart_banking_per_capita) "
     "SELECT geo, round(total_debt_per_capita_eur::numeric,0) AS debt_per_capita_eur, "
     "round(mortgage_per_capita_eur::numeric,0) AS mortgage_per_capita_eur, "
     "round(deposits_to_debt_ratio::numeric,2) AS deposits_to_debt_ratio "
     "FROM eurostat.mart_banking_per_capita, l WHERE year=l.y "
     "AND geo IN ('SE','DE','DK','NL','FI','FR') ORDER BY debt_per_capita_eur DESC",
     table()),

    ("Act 2 — A lost half-decade: cumulative real-income growth 2020–2025 (%)",
     "SELECT geo, round((exp(sum(ln(1+real_income_growth_pct/100)))*100-100)::numeric,1) "
     "AS cum_real_income_pct FROM eurostat.mart_cost_of_living "
     "WHERE year BETWEEN 2020 AND 2025 AND real_income_growth_pct IS NOT NULL "
     "AND geo IN ('SE','DE','DK','NL','FR','IT','ES','FI') "
     "GROUP BY geo HAVING count(*)=6 ORDER BY cum_real_income_pct",
     bar(["geo"], ["cum_real_income_pct"])),

    # ── Act 3: the state holds — the buffer the euro-south lacks ────────────
    ("Act 3 — The state's shield: public debt, Nordics vs the euro-south "
     "(% of GDP, latest)",
     "WITH l AS (SELECT max(year) y FROM eurostat.mart_sovereign_risk) "
     "SELECT geo, max(debt_pct_gdp) AS debt_pct_gdp "
     "FROM eurostat.mart_sovereign_risk, l WHERE year=l.y "
     "AND geo IN ('SE','DK','NL','DE','FI','PT','ES','FR','IT') "
     "GROUP BY geo ORDER BY debt_pct_gdp",
     bar(["geo"], ["debt_pct_gdp"])),

    # ── Payoff: who won — the diversified saver ─────────────────────────────
    ("Payoff — Who came out ahead: the diversified saver "
     "(real household wealth index, 2015 = 100)",
     "SELECT year, geo, real_wealth_index FROM eurostat.mart_portfolio_outcome "
     "WHERE geo IN ('SE','DE') AND real_wealth_index IS NOT NULL ORDER BY year, geo",
     line(["year", "geo"], ["real_wealth_index"])),
]


def main():
    global S
    S = api("POST", "/api/session", {"username": EMAIL, "password": PASSWORD})["id"]
    dbs = api("GET", "/api/database")
    dbs = dbs.get("data", dbs) if isinstance(dbs, dict) else dbs
    db_id = next(d["id"] for d in dbs if d["engine"] == "postgres")
    print(f"postgres db id = {db_id}")

    print(f"==> {DASHBOARD_NAME}")
    card_ids = []
    for title, sql, viz in CARDS:
        c = api("POST", "/api/card", {
            "name": title,
            "dataset_query": {"type": "native", "native": {"query": sql}, "database": db_id},
            "display": viz["display"],
            "visualization_settings": viz["visualization_settings"],
        })
        card_ids.append(c["id"])
        print(f"   - {title[:60]}… (card {c['id']})")

    dash = api("POST", "/api/dashboard", {"name": DASHBOARD_NAME, "description": DASHBOARD_DESC})
    did = dash["id"]
    # 2-wide grid, each card 12 cols x 8 rows.
    payload = [{"id": -(i + 1), "card_id": cid,
                "col": (i % 2) * 12, "row": (i // 2) * 8,
                "size_x": 12, "size_y": 8} for i, cid in enumerate(card_ids)]
    api("PUT", f"/api/dashboard/{did}/cards", {"cards": payload})
    pub = api("POST", f"/api/dashboard/{did}/public_link")
    url = f"{MB}/public/dashboard/{pub['uuid']}"

    print("\n=== DASHBOARD ===")
    print(f"{DASHBOARD_NAME}\n{url}")
    print(f"\nembed: {url}#bordered=false&titled=true")


if __name__ == "__main__":
    main()
