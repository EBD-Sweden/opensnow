#!/usr/bin/env python3
"""Build the analytical-story dashboards in Metabase via the REST API.

Creates one dashboard per story (household banking & savings, cost-of-living
squeeze, rates & housing), each with native-SQL cards over the eurostat marts,
lays them out 2x2, enables a public link, and prints the public URLs.

Env: MB_URL, MB_EMAIL, MB_PASSWORD
"""
import json
import os
import sys
import urllib.error
import urllib.request

MB = os.environ.get("MB_URL", "https://metabase.ebdsweden.com").rstrip("/")
EMAIL = os.environ["MB_EMAIL"]
PASSWORD = os.environ["MB_PASSWORD"]
S = None


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


def scatter(dims, metrics):
    return {"display": "scatter", "visualization_settings": {"graph.dimensions": dims, "graph.metrics": metrics}}


def table():
    return {"display": "table", "visualization_settings": {}}


# (title, sql, viz) per dashboard
STORIES = {
    "Household Banking & Savings": [
        ("Bank deposits per capita — latest (EUR)",
         "WITH l AS (SELECT max(year*10+quarter) m FROM eurostat.mart_household_banking) "
         "SELECT geo, deposits_per_capita_eur FROM eurostat.mart_household_banking, l "
         "WHERE year*10+quarter=l.m ORDER BY deposits_per_capita_eur DESC",
         bar(["geo"], ["deposits_per_capita_eur"])),
        ("Household debt per capita — latest (EUR)",
         "WITH l AS (SELECT max(year*10+quarter) m FROM eurostat.mart_household_banking) "
         "SELECT geo, debt_per_capita_eur FROM eurostat.mart_household_banking, l "
         "WHERE year*10+quarter=l.m ORDER BY debt_per_capita_eur DESC",
         bar(["geo"], ["debt_per_capita_eur"])),
        ("Where households keep their money — % of financial assets (latest)",
         "WITH l AS (SELECT max(year*10+quarter) m FROM eurostat.mart_household_asset_mix) "
         "SELECT geo, "
         "max(CASE WHEN asset_class='Deposits & cash' THEN pct_of_assets END) AS deposits, "
         "max(CASE WHEN asset_class='Equity & fund shares' THEN pct_of_assets END) AS equity, "
         "max(CASE WHEN asset_class='Insurance & pensions' THEN pct_of_assets END) AS insurance_pensions, "
         "max(CASE WHEN asset_class='Bonds' THEN pct_of_assets END) AS bonds "
         "FROM eurostat.mart_household_asset_mix, l WHERE year*10+quarter=l.m GROUP BY geo ORDER BY deposits",
         bar(["geo"], ["deposits", "equity", "insurance_pensions", "bonds"], stacked=True)),
        ("Household saving rate over time (%)",
         "SELECT (year||'-Q'||quarter) AS period, geo, saving_rate_pct "
         "FROM eurostat.mart_household_savings WHERE geo IN ('SE','DE','FR','NL') "
         "AND saving_rate_pct IS NOT NULL ORDER BY year, quarter",
         line(["period", "geo"], ["saving_rate_pct"])),
    ],
    "Cost-of-Living Squeeze": [
        ("Headline inflation over time (HICP, annual %)",
         "SELECT (year||'-'||lpad(month::text,2,'0')) AS period, geo, headline_pct "
         "FROM eurostat.mart_inflation WHERE geo IN ('SE','DE','FR','IT') "
         "AND headline_pct IS NOT NULL ORDER BY year, month",
         line(["period", "geo"], ["headline_pct"])),
        ("Sweden: real income growth vs inflation vs house prices (%)",
         "SELECT year, real_income_growth_pct, inflation_pct, house_price_yoy_pct "
         "FROM eurostat.mart_cost_of_living WHERE geo='SE' ORDER BY year",
         line(["year"], ["real_income_growth_pct", "inflation_pct", "house_price_yoy_pct"])),
        ("The squeeze by country — latest year",
         "WITH l AS (SELECT max(year) y FROM eurostat.mart_cost_of_living WHERE inflation_pct IS NOT NULL) "
         "SELECT geo, inflation_pct, real_income_growth_pct, house_price_yoy_pct "
         "FROM eurostat.mart_cost_of_living, l WHERE year=l.y ORDER BY inflation_pct DESC",
         table()),
        ("Sweden: inflation breakdown — energy / food / services (%)",
         "SELECT (year||'-'||lpad(month::text,2,'0')) AS period, energy_pct, food_pct, services_pct "
         "FROM eurostat.mart_inflation WHERE geo='SE' AND headline_pct IS NOT NULL ORDER BY year, month",
         line(["period"], ["energy_pct", "food_pct", "services_pct"])),
    ],
    "Rates & Housing": [
        ("Germany: short (3m) vs long (10y) interest rates (%)",
         "SELECT (year||'-Q'||quarter) AS period, short_rate_3m_pct, long_rate_10y_pct "
         "FROM eurostat.mart_rates_vs_housing WHERE geo='DE' "
         "AND (short_rate_3m_pct IS NOT NULL OR long_rate_10y_pct IS NOT NULL) ORDER BY year, quarter",
         line(["period"], ["short_rate_3m_pct", "long_rate_10y_pct"])),
        ("House price index over time (2015=100)",
         "SELECT (year||'-Q'||quarter) AS period, geo, hpi_2015_100 "
         "FROM eurostat.mart_rates_vs_housing WHERE geo IN ('SE','DE','FR','NL') "
         "AND hpi_2015_100 IS NOT NULL ORDER BY year, quarter",
         line(["period", "geo"], ["hpi_2015_100"])),
        ("Household debt per capita over time (EUR)",
         "SELECT (year||'-Q'||quarter) AS period, geo, debt_per_capita_eur "
         "FROM eurostat.mart_rates_vs_housing WHERE geo IN ('SE','DE','FR','NL') "
         "AND debt_per_capita_eur IS NOT NULL ORDER BY year, quarter",
         line(["period", "geo"], ["debt_per_capita_eur"])),
        ("Long-term rate vs house price index (scatter)",
         "SELECT long_rate_10y_pct, hpi_2015_100, geo FROM eurostat.mart_rates_vs_housing "
         "WHERE long_rate_10y_pct IS NOT NULL AND hpi_2015_100 IS NOT NULL",
         scatter(["long_rate_10y_pct"], ["hpi_2015_100"])),
    ],
}

POSITIONS = [(0, 0), (12, 0), (0, 8), (12, 8)]


def main():
    global S
    S = api("POST", "/api/session", {"username": EMAIL, "password": PASSWORD})["id"]
    dbs = api("GET", "/api/database")
    dbs = dbs.get("data", dbs) if isinstance(dbs, dict) else dbs
    db_id = next(d["id"] for d in dbs if d["engine"] == "postgres")
    print(f"postgres db id = {db_id}")

    results = []
    for name, cards in STORIES.items():
        print(f"==> {name}")
        card_ids = []
        for title, sql, viz in cards:
            c = api("POST", "/api/card", {
                "name": title,
                "dataset_query": {"type": "native", "native": {"query": sql}, "database": db_id},
                "display": viz["display"],
                "visualization_settings": viz["visualization_settings"],
            })
            card_ids.append(c["id"])
            print(f"   - {title} (card {c['id']})")
        dash = api("POST", "/api/dashboard", {"name": name,
                   "description": "Built by OpenSnow over Eurostat — dbt marts served via Postgres."})
        did = dash["id"]
        payload = [{"id": -(i + 1), "card_id": cid, "col": POSITIONS[i][0], "row": POSITIONS[i][1],
                    "size_x": 12, "size_y": 8} for i, cid in enumerate(card_ids)]
        api("PUT", f"/api/dashboard/{did}/cards", {"cards": payload})
        pub = api("POST", f"/api/dashboard/{did}/public_link")
        url = f"{MB}/public/dashboard/{pub['uuid']}"
        results.append((name, url))
        print(f"   public: {url}")

    print("\n=== DASHBOARDS ===")
    for name, url in results:
        print(f"{name}\t{url}")


if __name__ == "__main__":
    main()
