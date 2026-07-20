#!/usr/bin/env python3
"""Rebuild "The Krona's Bargain" as a Europe-wide, league-table read.

Each act becomes a ranking across ~27-30 countries with Sweden highlighted
(a two-series colour split: Sweden cyan, others grey). Reuses dashboard 16
identified by OPENSNOW_KRONA_PUBLIC_UUID so the dashboard URL + site listing are unchanged,
interleaves the step-by-step text cards, enables a public link per chart, and
prints the per-card UUIDs for the blog.

Env: MB_URL, MB_EMAIL, MB_PASSWORD, OPENSNOW_KRONA_PUBLIC_UUID
"""
import json, os, sys, urllib.request, urllib.error

MB = os.environ.get("MB_URL", "http://localhost:3000").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]; PASSWORD = os.environ["MB_PASSWORD"]
PUBLIC_UUID = os.environ["OPENSNOW_KRONA_PUBLIC_UUID"]
CYAN, GREY = "#22d3ee", "#5b6675"
S = None

def api(method, path, body=None):
    h = {"Content-Type": "application/json"}
    if S: h["X-Metabase-Session"] = S
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(f"{MB}{path}", data=data, headers=h, method=method)
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            raw = r.read().decode(); return json.loads(raw) if raw else {}
    except urllib.error.HTTPError as e:
        print(f"  ! {method} {path} -> {e.code}: {e.read().decode()[:300]}", file=sys.stderr); raise

NO_AGG = "geo NOT IN ('EA19','EA20','EU27_2020','EU28')"

def highlight(inner, val, order):
    """Wrap a (geo,<val>) query so Sweden is a separate, cyan series."""
    return (f"WITH d AS ({inner}) SELECT geo, "
            f"CASE WHEN geo='SE' THEN {val} END AS \"Sweden\", "
            f"CASE WHEN geo<>'SE' THEN {val} END AS \"Other countries\" "
            f"FROM d ORDER BY {val} {order}")

def rowviz():
    return {"display": "row", "visualization_settings": {
        "graph.dimensions": ["geo"], "graph.metrics": ["Sweden", "Other countries"],
        "stackable.stack_type": "stacked",
        "series_settings": {"Sweden": {"color": CYAN}, "Other countries": {"color": GREY}}}}

def lineviz(metric):
    return {"display": "line", "visualization_settings": {"graph.dimensions": ["year"], "graph.metrics": [metric]}}

# key -> (name, sql, viz)
CARDS = {
 "krona": ("The weak krona — SEK per euro, year-end",
    "SELECT year, sek_per_eur FROM eurostat.mart_sek_vs_euro WHERE sek_per_eur IS NOT NULL ORDER BY year",
    lineviz("sek_per_eur")),
 "comp": ("Act 1 — Competitiveness gain: real exchange-rate change 2010→2025 (%, lower = more competitive)",
    highlight("SELECT f.geo, round((100.0*(l.v/f.v)-100)::numeric,1) AS reer FROM "
              "(SELECT geo, reer_cpi_index v FROM eurostat.mart_competitiveness WHERE year=2010 AND reer_cpi_index IS NOT NULL) f "
              "JOIN (SELECT geo, reer_cpi_index v FROM eurostat.mart_competitiveness WHERE year=2025 AND reer_cpi_index IS NOT NULL) l "
              f"USING(geo) WHERE f.{NO_AGG}", "reer", "ASC"), rowviz()),
 "debt": ("Act 2 — Household debt per capita (latest, €)",
    highlight("WITH y AS (SELECT max(year) m FROM eurostat.mart_banking_per_capita) "
              "SELECT geo, round(total_debt_per_capita_eur::numeric,0) AS debt FROM eurostat.mart_banking_per_capita, y "
              f"WHERE year=y.m AND total_debt_per_capita_eur IS NOT NULL AND {NO_AGG}", "debt", "DESC"), rowviz()),
 "income": ("Act 2 — A lost half-decade: cumulative real-income growth 2020→2025 (%)",
    highlight("SELECT geo, round((exp(sum(ln(1+real_income_growth_pct/100)))*100-100)::numeric,1) AS inc "
              "FROM eurostat.mart_cost_of_living WHERE year BETWEEN 2020 AND 2025 AND real_income_growth_pct IS NOT NULL "
              f"AND {NO_AGG} GROUP BY geo HAVING count(*)=6", "inc", "ASC"), rowviz()),
 "pubdebt": ("Act 3 — Public debt, % of GDP (latest)",
    highlight("WITH y AS (SELECT max(year) m FROM eurostat.mart_sovereign_risk) "
              "SELECT geo, round(max(debt_pct_gdp)::numeric,1) AS pd FROM eurostat.mart_sovereign_risk, y "
              f"WHERE year=y.m AND debt_pct_gdp IS NOT NULL AND {NO_AGG} GROUP BY geo", "pd", "ASC"), rowviz()),
 "wealth": ("Payoff — Real household wealth index, 2015 → latest (2015 = 100)",
    highlight("WITH y AS (SELECT max(year) m FROM eurostat.mart_portfolio_outcome) "
              "SELECT geo, round(real_wealth_index::numeric,0) AS w FROM eurostat.mart_portfolio_outcome, y "
              f"WHERE year=y.m AND real_wealth_index IS NOT NULL AND {NO_AGG}", "w", "DESC"), rowviz()),
}

def main():
    global S
    S = api("POST", "/api/session", {"username": EMAIL, "password": PASSWORD})["id"]
    db_id = next(d["id"] for d in api("GET", "/api/database").get("data", []) if d["engine"] == "postgres")
    dash_id = next(d["id"] for d in api("GET", "/api/dashboard") if d.get("public_uuid") == PUBLIC_UUID)

    ids, uuids = {}, {}
    for key, (name, sql, viz) in CARDS.items():
        c = api("POST", "/api/card", {"name": name,
            "dataset_query": {"type": "native", "native": {"query": sql}, "database": db_id},
            "display": viz["display"], "visualization_settings": viz["visualization_settings"]})
        ids[key] = c["id"]
        uuids[key] = api("POST", f"/api/card/{c['id']}/public_link").get("uuid")
        print(f"  {key}: card {c['id']}  uuid {uuids[key]}")

    payload = []; n = 0; row = 0
    def text(md, sy):
        nonlocal row, n; n += 1
        payload.append({"id": -n, "card_id": None, "row": row, "col": 0, "size_x": 24, "size_y": sy,
                        "visualization_settings": {"text": md}}); row += sy
    def chart(key, sy):
        nonlocal row, n; n += 1
        payload.append({"id": -n, "card_id": ids[key], "row": row, "col": 0, "size_x": 24, "size_y": sy,
                        "visualization_settings": {}}); row += sy

    text("## 🇸🇪 The Krona's Bargain — a Europe-wide, step-by-step read\n"
         "**A weak currency is a wealth transfer.** Built on **OpenSnow** over Eurostat, ECB & market data. "
         "We rank every European country on four dimensions — and watch where Sweden (in cyan) lands.", 3)
    text("### The setup · A weak krona\n👉 **Look for:** the ~20% climb in SEK per euro, 2010–2023.", 2)
    chart("krona", 6)
    text("### Act 1 · The firm wins — Sweden is 2nd-most-competitive in Europe\n"
         "👉 **Look for:** Sweden near the top (only Norway gained more), while euro-anchored Germany got *dearer*.", 2)
    chart("comp", 11)
    text("### Act 2 · The household pays — heavily indebted, worst income recovery\n"
         "👉 **Look for:** Sweden among Europe's 4 most-indebted households — yet near the *bottom* on real-income growth.", 2)
    chart("debt", 10); chart("income", 9)
    text("### Act 3 · The state holds — among Europe's lowest public debt\n"
         "👉 **Look for:** Sweden in the low-debt cluster, far from the euro-south (Italy, Greece, France).", 2)
    chart("pubdebt", 11)
    text("### Payoff · The diversified saver — top-tier wealth, far above Germany\n"
         "👉 **Look for:** Sweden in the top group on real wealth; Germany (cash-heavy) sits well below.", 2)
    chart("wealth", 10)

    api("PUT", f"/api/dashboard/{dash_id}/cards", {"cards": payload})
    print(f"\nDASHBOARD {dash_id} -> {MB}/public/dashboard/{PUBLIC_UUID}")
    print("BLOG_UUIDS=" + json.dumps(uuids))

if __name__ == "__main__":
    main()
