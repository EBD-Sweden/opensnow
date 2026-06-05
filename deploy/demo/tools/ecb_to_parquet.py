#!/usr/bin/env python3
"""Fetch real banking data from the ECB Data Portal into tidy Parquet.

Eurostat has no card/ATM/consumer-credit data — the ECB does. This pulls it via
the free ECB Data Portal REST API (https://data-api.ecb.europa.eu, SDMX-CSV, no
auth) and writes three long-format Parquet files for the demo:

  ecb_cards.parquet  geo, year, metric, value   (PSS, annual)
      metric ∈ credit_count|credit_value|debit_count|debit_value|atm_count|atm_value
  ecb_bsi.parquet    geo, year, metric, value   (BSI, year-end stock, EUR mn)
      metric ∈ consumer_credit|house_purchase|deposits
  ecb_mir.parquet    geo, year, metric, rate    (MIR, annual avg %)
      metric ∈ consumer_rate|mortgage_rate

Usage: ecb_to_parquet.py <out_dir> [--start-year 2010]
"""
import csv
import io
import sys
import urllib.request
from collections import defaultdict
from pathlib import Path

import pandas as pd

API = "https://data-api.ecb.europa.eu/service/data"

# Card payments + ATM cash withdrawals (PSS, annual). {} = REF_AREA (country).
PSS = {
    "credit_count": "PSS/A.{}.F000.I13.Z00Z.NT.X0.20.Z0Z.Z",
    "credit_value": "PSS/A.{}.F000.I13.Z00Z.VT.X0.20.Z01.E",
    "debit_count": "PSS/A.{}.F000.I12.Z00Z.NT.X0.20.Z0Z.Z",
    "debit_value": "PSS/A.{}.F000.I12.Z00Z.VT.X0.20.Z01.E",
    "atm_count": "PSS/A.{}.F100.I10.I111.NT.X0.20.Z0Z.Z",
    "atm_value": "PSS/A.{}.F100.I10.I111.VT.X0.20.Z01.E",
}
# MFI balance sheet items (BSI, monthly stocks, EUR mn). Households (2250).
BSI = {
    "consumer_credit": "BSI/M.{}.N.A.A21.A.1.U2.2250.Z01.E",
    "house_purchase": "BSI/M.{}.N.A.A22.A.1.U2.2250.Z01.E",
    "deposits": "BSI/M.{}.N.A.L20.A.1.U2.2250.Z01.E",
}
# MFI interest rates (MIR, monthly %, new business to households).
MIR = {
    "consumer_rate": "MIR/M.{}.B.A2B.A.R.A.2250.EUR.N",
    "mortgage_rate": "MIR/M.{}.B.A2C.A.R.A.2250.EUR.N",
}

PSS_COUNTRIES = ["DE", "FR", "IT", "ES", "NL", "FI", "SE", "DK", "PL", "AT", "BE", "PT", "IE"]
EA_COUNTRIES = ["DE", "FR", "IT", "ES", "NL", "FI", "AT", "BE", "PT", "IE"]  # BSI/MIR are euro-area


def fetch(path, start_year):
    url = f"{API}/{path}?startPeriod={start_year}-01-01&format=csvdata"
    try:
        with urllib.request.urlopen(url, timeout=60) as r:
            return list(csv.DictReader(io.StringIO(r.read().decode())))
    except Exception:
        return []


def collect(spec, countries, start_year, annual_agg):
    """Return rows [{geo, year, metric, value}] aggregated to annual."""
    out = []
    for metric, tmpl in spec.items():
        for geo in countries:
            rows = fetch(tmpl.format(geo), start_year)
            by_year = defaultdict(list)
            for r in rows:
                tp, v = r.get("TIME_PERIOD", ""), r.get("OBS_VALUE", "")
                if not tp or v in ("", None):
                    continue
                try:
                    by_year[int(tp[:4])].append((tp, float(v)))
                except ValueError:
                    continue
            for year, obs in by_year.items():
                out.append({"geo": geo, "year": year, "metric": metric,
                            "value": annual_agg(obs)})
    return out


def last_value(obs):  # year-end stock: latest period in the year
    return sorted(obs)[-1][1]


def mean_value(obs):  # annual average (rates)
    return round(sum(v for _, v in obs) / len(obs), 3)


def main(argv):
    out_dir = Path(argv[0]) if argv else Path(".")
    start = 2010
    if "--start-year" in argv:
        start = int(argv[argv.index("--start-year") + 1])
    out_dir.mkdir(parents=True, exist_ok=True)

    cards = collect(PSS, PSS_COUNTRIES, start, last_value)  # annual data already
    bsi = collect(BSI, EA_COUNTRIES, start, last_value)
    mir = collect(MIR, EA_COUNTRIES, start, mean_value)

    for name, rows, vcol in [("ecb_cards", cards, "value"),
                             ("ecb_bsi", bsi, "value"),
                             ("ecb_mir", mir, "rate")]:
        df = pd.DataFrame(rows)
        if vcol == "rate" and "value" in df:
            df = df.rename(columns={"value": "rate"})
        df["dataset"] = name
        df.to_parquet(out_dir / f"{name}.parquet", index=False)
        n_geo = df["geo"].nunique() if len(df) else 0
        print(f"{name}: {len(df)} rows, {n_geo} countries -> {out_dir / (name + '.parquet')}")


if __name__ == "__main__":
    main(sys.argv[1:])
