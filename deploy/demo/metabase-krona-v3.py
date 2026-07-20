#!/usr/bin/env python3
"""The Krona's Bargain — focused 4-country comparison (supersedes v2).

Sweden vs Denmark, Germany & France — clean coloured line charts over time
(Sweden cyan, Denmark green, Germany coral, France gold) instead of the busy
27-country league tables. Rebuilds an operator-selected existing public dashboard with the
step-by-step text, enables a public link per chart, prints the per-card UUIDs.

Env: MB_URL, MB_EMAIL, MB_PASSWORD, OPENSNOW_KRONA_PUBLIC_UUID
"""
import json, os, sys, urllib.request, urllib.error

MB = os.environ.get("MB_URL", "http://localhost:3000").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]; PASSWORD = os.environ["MB_PASSWORD"]
PUBLIC_UUID = os.environ["OPENSNOW_KRONA_PUBLIC_UUID"]
COLORS = {"SE": "#22d3ee", "DK": "#34d399", "DE": "#fb7185", "FR": "#fbbf24"}
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

def multiline(metric):
    return {"display": "line", "visualization_settings": {
        "graph.dimensions": ["year", "geo"], "graph.metrics": [metric],
        "series_settings": {g: {"color": c} for g, c in COLORS.items()}}}

IN = "geo IN ('SE','DK','DE','FR')"

CARDS = {
 "krona": ("The weak krona — SEK per euro, year-end",
    "SELECT year, sek_per_eur FROM eurostat.mart_sek_vs_euro WHERE sek_per_eur IS NOT NULL ORDER BY year",
    {"display": "line", "visualization_settings": {"graph.dimensions": ["year"], "graph.metrics": ["sek_per_eur"],
        "series_settings": {"sek_per_eur": {"color": "#22d3ee"}}}}),
 "reer": ("Act 1 — Real effective exchange rate (2015=100, lower = more competitive)",
    f"SELECT year, geo, reer_cpi_index FROM eurostat.mart_competitiveness WHERE {IN} AND reer_cpi_index IS NOT NULL ORDER BY geo, year",
    multiline("reer_cpi_index")),
 "ulc": ("Act 1 — Unit labour costs (2015=100): wages rose alike",
    f"SELECT year, geo, ulc_index_2015 FROM eurostat.mart_competitiveness WHERE {IN} AND ulc_index_2015 IS NOT NULL ORDER BY geo, year",
    multiline("ulc_index_2015")),
 "debt": ("Act 2 — Household debt per capita (€)",
    f"SELECT year, geo, total_debt_per_capita_eur FROM eurostat.mart_banking_per_capita WHERE {IN} AND total_debt_per_capita_eur IS NOT NULL ORDER BY geo, year",
    multiline("total_debt_per_capita_eur")),
 "income": ("Act 2 — Real household income (2019=100): a lost half-decade",
    "WITH base AS (SELECT DISTINCT geo, 2019 AS year, 100.0 AS real_income_index FROM eurostat.mart_cost_of_living WHERE "
    f"{IN}), gr AS (SELECT year, geo, real_income_growth_pct FROM eurostat.mart_cost_of_living WHERE {IN} "
    "AND year BETWEEN 2020 AND 2025 AND real_income_growth_pct IS NOT NULL), "
    "idx AS (SELECT year, geo, round((100*exp(sum(ln(1+real_income_growth_pct/100)) OVER (PARTITION BY geo ORDER BY year)))::numeric,1) AS real_income_index FROM gr) "
    "SELECT year, geo, real_income_index FROM base UNION ALL SELECT year, geo, real_income_index FROM idx ORDER BY geo, year",
    multiline("real_income_index")),
 "pubdebt": ("Act 3 — Public debt, % of GDP",
    f"SELECT year, geo, round(avg(debt_pct_gdp)::numeric,1) AS debt_pct_gdp FROM eurostat.mart_sovereign_risk WHERE {IN} AND debt_pct_gdp IS NOT NULL GROUP BY year, geo ORDER BY geo, year",
    multiline("debt_pct_gdp")),
 "wealth": ("Payoff — Real household wealth index (2015=100)",
    f"SELECT year, geo, real_wealth_index FROM eurostat.mart_portfolio_outcome WHERE {IN} AND real_wealth_index IS NOT NULL ORDER BY geo, year",
    multiline("real_wealth_index")),
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
        ids[key] = c["id"]; uuids[key] = api("POST", f"/api/card/{c['id']}/public_link").get("uuid")
        print(f"  {key}: card {c['id']}  uuid {uuids[key]}")

    payload = []; n = 0; row = 0
    def text(md, sy):
        nonlocal row, n; n += 1
        payload.append({"id": -n, "card_id": None, "row": row, "col": 0, "size_x": 24, "size_y": sy,
                        "visualization_settings": {"text": md}}); row += sy
    def chart(key, sy=7):
        nonlocal row, n; n += 1
        payload.append({"id": -n, "card_id": ids[key], "row": row, "col": 0, "size_x": 24, "size_y": sy,
                        "visualization_settings": {}}); row += sy

    text("## 🇸🇪 The Krona's Bargain — a step-by-step read\n"
         "**A weak currency is a wealth transfer.** Built on **OpenSnow** over Eurostat, ECB & market data. "
         "Four economies, four doors: **Sweden** (its own, floating krona), **Denmark** (krone, pegged to the euro), "
         "and the euro core — **Germany** & **France**.", 3)
    text("### The setup · A weak krona\n👉 **Look for:** the ~20% climb in SEK per euro, 2010–2023.", 2)
    chart("krona", 6)
    text("### Act 1 · The firm wins — the currency did the work, not wages\n"
         "👉 **Look for:** Sweden's real exchange rate (left) *falls* while Germany's rises — yet unit labour "
         "costs (right) climb alike. The krona, not wage restraint, kept Sweden cheap.", 2)
    chart("reer"); chart("ulc")
    text("### Act 2 · The household pays — through debt, not the supermarket\n"
         "👉 **Look for:** Sweden & Denmark carry far more household debt than Germany or France (left), and "
         "Swedish real income flatlined after 2020 (right).", 2)
    chart("debt"); chart("income")
    text("### Act 3 · The state holds — the buffer the euro core lacks\n"
         "👉 **Look for:** Sweden & Denmark near 30–35% of GDP; France climbing past 110%.", 2)
    chart("pubdebt")
    text("### Payoff · The diversified saver won\n"
         "👉 **Look for:** the two Nordic savers (Sweden, Denmark) pulling clear of cash-heavy Germany; a weak "
         "krona lifts foreign assets.", 2)
    chart("wealth")

    api("PUT", f"/api/dashboard/{dash_id}/cards", {"cards": payload})
    print(f"\nDASHBOARD {dash_id} -> {MB}/public/dashboard/{PUBLIC_UUID}")
    print("BLOG_UUIDS=" + json.dumps(uuids))

if __name__ == "__main__":
    main()
