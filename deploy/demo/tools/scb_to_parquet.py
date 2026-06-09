#!/usr/bin/env python3
"""Ingest Swedish financial-market statistics from SCB (Statistics Sweden) into
tidy Parquet — the authoritative source for Swedish household credit that the
euro-area ECB data and the Eurostat maturity proxy cannot give correctly.

SCB's Financial Market Statistics (FM5001) splits household lending by *collateral*
(housing = mortgage, unsecured = consumer credit, vehicles, …) and lending rates
by *purpose* (consumption vs housing). Free PxWeb JSON API, no key, monthly back
to 1987/2001.  Docs: https://www.scb.se/en/ ; API: https://api.scb.se/OV0104/v1/

Output: one Parquet per table under the given dir, e.g. scb_household_loans_by_collateral.parquet
        Columns: geo='SE', <dimension columns>, measure, value, period, year, month, dataset
"""
import json
import sys
import time
import urllib.request
from pathlib import Path

import pandas as pd

BASE = "https://api.scb.se/OV0104/v1/doris/en/ssd/"
CELL_LIMIT = 90000  # SCB caps response cells per call; chunk time to stay under it.

# full table path (under .../ssd/) -> output name. We pull ALL values of every
# dimension (full history, every loan/collateral/purpose/expenditure type).
TABLES = {
    # Financial Market Statistics — credit, rates, money
    "FM/FM5001/FM5001A/FM5001Sakerhet": "scb_household_loans_by_collateral",
    "FM/FM5001/FM5001C/RantaT03N": "scb_lending_rates_by_purpose",
    "FM/FM5001/FM5001C/RantaT04N": "scb_housing_loan_rates_by_fixation",
    "FM/FM5001/FM5001C/RantaT01N": "scb_lending_rates_by_fixation",
    "FM/FM5001/FM5001C/RantaT05": "scb_deposit_rates",
    "FM/FM5001/FM5001A/FM5001penningmangd": "scb_money_supply",
    "FM/FM5001/FM5001A/FM5001SDDSMFI": "scb_mfi_balance_sheet",
    # Household budget survey — WHAT households spend on (COICOP categories) + income
    "HE/HE0201/HE0201D/HUThush": "scb_household_expenditure_by_type",
    "HE/HE0201/HE0201D/HUTupplat": "scb_household_expenditure_by_tenure",
    "HE/HE0201/HE0201A/HUTutgift5": "scb_household_expenditure_by_income_decile",
}


def snake(text):
    out = "".join(c.lower() if c.isalnum() else "_" for c in text)
    while "__" in out:
        out = out.replace("__", "_")
    return out.strip("_")[:40] or "dim"


def get_json(url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(
        url, data=data, headers={"content-type": "application/json"}, method="POST" if data else "GET"
    )
    with urllib.request.urlopen(req, timeout=90) as r:
        return json.load(r)


def fetch_table(path, name, out_dir):
    meta = get_json(BASE + path)
    variables = meta["variables"]
    # code -> {valuecode: valuetext} and the ordered value-code list per variable
    code2text = {}
    codes = {}
    time_var = None
    for v in variables:
        vals = v.get("values", [])
        texts = v.get("valueTexts", vals)
        code2text[v["code"]] = dict(zip(vals, texts))
        codes[v["code"]] = vals
        if v["code"].lower() == "tid":
            time_var = v["code"]
    # dimension (non-time, non-content) variables keep their full value lists.
    non_time = [v["code"] for v in variables if v["code"] != time_var and v["code"] != "ContentsCode"]
    non_time_product = 1
    for c in non_time:
        non_time_product = max(non_time_product, 1) * max(1, len(codes[c]))
    n_content = max(1, len(codes.get("ContentsCode", [1])))
    per_call = max(1, CELL_LIMIT // (non_time_product * n_content))
    tid_codes = codes[time_var]
    chunks = [tid_codes[i : i + per_call] for i in range(0, len(tid_codes), per_call)]

    var_text = {v["code"]: snake(v["text"]) for v in variables}
    rows = []
    for ci, chunk in enumerate(chunks):
        query = []
        for v in variables:
            sel = chunk if v["code"] == time_var else codes[v["code"]]
            query.append({"code": v["code"], "selection": {"filter": "item", "values": sel}})
        resp = get_json(BASE + path, {"query": query, "response": {"format": "json"}})
        cols = resp["columns"]
        dim_cols = [c for c in cols if c["type"] in ("d", "t")]
        content_cols = [c for c in cols if c["type"] == "c"]
        for item in resp["data"]:
            base = {}
            for dc, kc in zip(dim_cols, item["key"]):
                if dc["code"] == time_var:
                    base["period"] = kc
                else:
                    base[var_text[dc["code"]]] = code2text.get(dc["code"], {}).get(kc, kc)
            for cc, raw in zip(content_cols, item["values"]):
                try:
                    val = float(raw)
                except (ValueError, TypeError):
                    val = None
                r = dict(base)
                r["measure"] = cc["text"]
                r["value"] = val
                rows.append(r)
        time.sleep(0.3)  # be polite to the API
        print(f"    {name}: chunk {ci + 1}/{len(chunks)} ({len(rows)} rows)")

    df = pd.DataFrame(rows)
    # parse period -> year, month. Handles monthly (YYYYMmm), quarterly (YYYYKq)
    # and annual (YYYY, e.g. budget-survey years); month is NaN for annual.
    per = df["period"].astype(str)
    df["year"] = per.str[:4].astype(int)
    sep = per.str[4:5]
    num = pd.to_numeric(per.str[5:], errors="coerce")
    df["month"] = num.where(sep == "M").fillna((num * 3).where(sep == "K"))
    df.insert(0, "geo", "SE")
    df["dataset"] = name
    df.to_parquet(out_dir / f"{name}.parquet", index=False)
    dims = [c for c in df.columns if c not in ("geo", "period", "year", "month", "value", "measure", "dataset")]
    print(
        f"  {name}: {len(df)} rows, {df['year'].min()}-{df['year'].max()}, dims={dims} -> {name}.parquet"
    )


def main(argv):
    out_dir = Path(argv[0]) if argv else Path(".")
    # Optional name filters: only fetch tables whose output name contains an arg.
    filters = argv[1:]
    for path, name in TABLES.items():
        if filters and not any(f in name for f in filters):
            continue
        try:
            fetch_table(path, name, out_dir)
        except Exception as e:
            print(f"  {name}: ERROR {e}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
