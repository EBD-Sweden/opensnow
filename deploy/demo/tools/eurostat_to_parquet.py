#!/usr/bin/env python3
"""Convert Eurostat SDMX-TSV bulk files into tidy (long) Parquet for the demo.

Eurostat TSV: the first column packs the dimension key as comma-separated codes
(header e.g. `freq,unit,sector,finpos,na_item,geo\\TIME_PERIOD`); the remaining
columns are one period each (`1998-Q4`, `1997-01`, `2020`). Cells may carry a
status flag (`123.4 p`) and `:` marks missing.

Output schema matches the demo's existing parquet: the dimension columns, then
`time_period, value, flag, year, period_type, sub_period, dataset`.

Usage:
  eurostat_to_parquet.py <src_dir> <out_dir> <code> [<code> ...] [--min-year N]
"""
import sys
from pathlib import Path

import numpy as np
import pandas as pd

# A representative EU set (core + Nordics + EU/EA aggregates) — keeps Parquet small.
COUNTRIES = {
    "SE", "DE", "FR", "ES", "NL", "IT", "FI", "DK", "PL", "AT", "BE", "IE",
    "PT", "NO", "EA20", "EA19", "EU27_2020",
}
DEFAULT_MIN_YEAR = 2010


def parse_period(tp: str):
    """Return (period_type, year, sub_period) for a Eurostat period label."""
    tp = tp.strip()
    year = int(tp[:4]) if tp[:4].isdigit() else None
    if "Q" in tp:
        return "Q", year, int(tp.split("Q")[-1])
    rest = tp[5:] if len(tp) > 5 and tp[4] == "-" else ""
    if rest.isdigit():  # YYYY-MM monthly
        return "M", year, int(rest)
    return "A", year, 0


def convert(code: str, src_dir: Path, out_dir: Path, min_year: int) -> int:
    df = pd.read_csv(src_dir / f"{code}.tsv", sep="\t", dtype=str)
    first = df.columns[0]
    dim_names = first.split("\\")[0].split(",")
    dims = df[first].str.split(",", expand=True)
    dims.columns = dim_names
    time_cols = [c for c in df.columns if c != first]

    long = pd.concat([dims, df[time_cols]], axis=1).melt(
        id_vars=dim_names, var_name="time_period", value_name="raw"
    )
    long["time_period"] = long["time_period"].str.strip()
    long = long[long["geo"].isin(COUNTRIES)]

    raw = long["raw"].fillna("").str.strip()
    long["value"] = pd.to_numeric(raw.str.extract(r"(-?\d+\.?\d*)")[0], errors="coerce")
    long["flag"] = raw.str.extract(r"[\d.\s:]*([a-z]+)")[0].replace("", np.nan)
    long = long.dropna(subset=["value"])

    parsed = long["time_period"].map(parse_period)
    long["year"] = [p[1] for p in parsed]
    long["period_type"] = [p[0] for p in parsed]
    long["sub_period"] = [p[2] for p in parsed]
    long = long[long["year"].notna() & (long["year"] >= min_year)]
    long["year"] = long["year"].astype(int)
    long["dataset"] = code

    cols = dim_names + ["time_period", "value", "flag", "year", "period_type", "sub_period", "dataset"]
    long = long[cols].reset_index(drop=True)
    out = out_dir / f"{code}.parquet"
    long.to_parquet(out, index=False)
    print(f"{code}: {len(long):>8} rows, {len(dim_names)} dims -> {out}")
    return len(long)


def main(argv):
    if len(argv) < 3:
        sys.exit(__doc__)
    src_dir, out_dir = Path(argv[0]), Path(argv[1])
    out_dir.mkdir(parents=True, exist_ok=True)
    min_year = DEFAULT_MIN_YEAR
    codes = []
    i = 2
    while i < len(argv):
        if argv[i] == "--min-year":
            min_year = int(argv[i + 1]); i += 2
        else:
            codes.append(argv[i]); i += 1
    for code in codes:
        convert(code, src_dir, out_dir, min_year)


if __name__ == "__main__":
    main(sys.argv[1:])
