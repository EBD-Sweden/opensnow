#!/usr/bin/env python3
"""Fetch national stock-market indices from the FMP API into tidy Parquet.

These are the real per-country equity benchmarks (Germany = DAX, Sweden =
OMX Stockholm 30, etc.) used as the *equity / fund* return in the Investing
Scorecard, replacing the earlier GDP-growth proxy. Local-currency price
indices, so the SEK-vs-euro angle stays in the FX mart, not here.

To put every country on the same footing (a fund investor's *total* return,
dividends reinvested) we add a flat assumed net dividend yield to the price
indices. The DAX is already a total-return index, so it gets no add-on.

The FMP key is read from $FMP_API_KEY (never hard-coded / committed).
Output: equity_index.parquet  [geo, year, value, div_yield_pct]
        (value = year-end index level; div_yield_pct = total-return add-on)
"""
import json
import os
import sys
import urllib.request
from pathlib import Path

import pandas as pd

# Eurostat geo code -> FMP index symbol. Codes match the other sources
# (UK, not GB; EA = euro area, served by the EURO STOXX 50 as the fallback
# benchmark for euro members without a clean national index, e.g. IE/PT).
SYMBOLS = {
    "DE": "^GDAXI",      # DAX
    "SE": "^OMXS30",     # OMX Stockholm 30
    "FR": "^FCHI",       # CAC 40
    "NL": "^AEX",        # AEX
    "FI": "^OMXH25",     # OMX Helsinki 25
    "IT": "FTSEMIB.MI",  # FTSE MIB
    "ES": "^IBEX",       # IBEX 35
    "DK": "^OMXC20",     # OMX Copenhagen 20
    "NO": "^OSEAX",      # Oslo All-Share
    "AT": "^ATX",        # ATX
    "BE": "^BFX",        # BEL 20
    "PL": "WIG20.WA",    # WIG 20
    "CH": "^SSMI",       # SMI
    "UK": "^FTSE",       # FTSE 100
    "EA": "^STOXX50E",   # EURO STOXX 50 (euro-area fallback)
}

# Indices that already reinvest dividends (total-return) get no add-on; the rest
# are price indices, to which we add a flat net dividend yield to approximate the
# total return a fund holder actually earns. (The DAX is the notable TR index.)
TOTAL_RETURN = {"DE"}
DIV_YIELD_PCT = 3.0


def fetch(symbol, key):
    url = (
        f"https://financialmodelingprep.com/api/v3/historical-price-full/{symbol}"
        f"?from=2013-01-01&to=2026-12-31&apikey={key}"
    )
    with urllib.request.urlopen(url, timeout=60) as r:
        d = json.load(r)
    return d.get("historical") if isinstance(d, dict) else (d or [])


def main(argv):
    key = os.environ.get("FMP_API_KEY") or os.environ.get("FMP_KEY")
    if not key:
        print("FMP_API_KEY not set", file=sys.stderr)
        return 1
    out_dir = Path(argv[0]) if argv else Path(".")
    rows = []
    for geo, sym in SYMBOLS.items():
        try:
            hist = fetch(sym, key)
        except Exception as e:
            print(f"  {geo} ({sym}): fetch error {e}")
            continue
        if not hist:
            print(f"  {geo} ({sym}): empty")
            continue
        df = pd.DataFrame(hist)[["date", "close"]]
        df["year"] = df["date"].str[:4].astype(int)
        # year-end level = the close on the last trading day of each year
        yend = df.sort_values("date").groupby("year", as_index=False).last()
        div = 0.0 if geo in TOTAL_RETURN else DIV_YIELD_PCT
        for _, r in yend.iterrows():
            rows.append((geo, int(r["year"]), round(float(r["close"]), 2), div))
        kind = "TR" if geo in TOTAL_RETURN else f"price +{DIV_YIELD_PCT:g}% div"
        print(f"  {geo} ({sym}): {len(yend)} years {int(yend['year'].min())}-{int(yend['year'].max())} [{kind}]")
    out = pd.DataFrame(rows, columns=["geo", "year", "value", "div_yield_pct"])
    out = out[out["year"] >= 2013].sort_values(["geo", "year"]).reset_index(drop=True)
    out.to_parquet(out_dir / "equity_index.parquet", index=False)
    print(f"equity_index: {len(out)} rows, {out['geo'].nunique()} geos -> {out_dir / 'equity_index.parquet'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
