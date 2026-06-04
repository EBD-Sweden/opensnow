# ClickBench Baseline — OpenSnow / DataFusion

This document captures the **methodology** and **expected performance ranges**
for the [ClickBench](https://github.com/ClickHouse/ClickBench) suite running
against OpenSnow's embedded DataFusion engine. No numbers are reported here;
those are produced on demand by `bench/clickbench.sh` against your own
hardware. The point of this file is to set expectations: how to read the
output and what "good" looks like.

## Setup

| Setting        | Value                                                    |
| -------------- | -------------------------------------------------------- |
| Dataset        | `hits.parquet`, 100 M rows, ~14.8 GB compressed Parquet  |
| Source         | `https://datasets.clickhouse.com/hits_compatible/`       |
| Storage        | Local filesystem (cold) or S3-compatible (warm cache)    |
| Engine         | OpenSnow (Apache DataFusion 45.x, single node, all CPUs) |
| Runs per query | 3 (1 cold, 2 warm) — report the warm median              |
| Caching        | OS page cache only; no DataFusion result cache           |

The script (`bench/clickbench.sh`) pulls the parquet file, registers it as an
external table, runs each of the 43 canonical queries three times, and writes
a CSV under `bench/results/`. It also runs the same queries against DuckDB
for an apples-to-apples local-engine comparison.

## Query categories

ClickBench is intentionally diverse. Internally the 43 queries fall into five
categories that exercise different engine paths.

| Cat | Queries     | What it stresses                                              |
| --- | ----------- | ------------------------------------------------------------- |
| A   | Q1–Q4       | Scalar aggregates over the full 100 M-row scan                |
| B   | Q5–Q7       | `COUNT(DISTINCT ...)` and min/max — tests HLL / sorted-runs   |
| C   | Q8–Q19      | `GROUP BY` of varying cardinality, with `ORDER BY ... LIMIT`  |
| D   | Q20–Q26     | Equality and `LIKE` filter pushdown, top-N sort               |
| E   | Q27–Q43     | Multi-column `GROUP BY`, large projections, nested predicates |

A healthy DataFusion baseline shows roughly:

| Cat | Cold (full scan) | Warm (cached) | Notes                                       |
| --- | ---------------- | ------------- | ------------------------------------------- |
| A   | 0.5 – 2.0 s      | 0.05 – 0.3 s  | Bound by Parquet decode; bandwidth-limited  |
| B   | 1.0 – 3.0 s      | 0.3 – 1.0 s   | Distinct counts dominate (hash table size)  |
| C   | 1.0 – 4.0 s      | 0.2 – 1.5 s   | High-cardinality GROUP BYs are the long pole|
| D   | 1.5 – 5.0 s      | 0.5 – 2.0 s   | LIKE on URLs — predicate pushdown matters   |
| E   | 2.0 – 8.0 s      | 0.5 – 3.0 s   | Q29 (90× SUM) and Q33 stress vectorisation  |

These are ranges for a recent x86 desktop (8–16 cores, 32 GB RAM, NVMe SSD).
On smaller machines or with cold S3, multiply by 2–4×. Anything outside the
"cold" range warrants investigation; warm runs that exceed the upper bound
usually mean a configuration regression (e.g. Parquet pushdown disabled, the
default `target_partitions` changed, or an object-store endpoint is mis-tuned).

## What we expect to see vs DuckDB

DataFusion ranked #1 fastest single-node engine on ClickBench in November 2024.
On the same hardware and the same Parquet file, OpenSnow should be at parity
with DuckDB to within 20–30% per query. Categories where we typically win:

* **A (full-scan aggregates)** — DataFusion's vectorised Parquet reader and
  cardinality-based projection pruning are tight.
* **D (filter + LIKE pushdown)** — predicate pushdown into the Parquet page
  filter is effective when the column is dictionary-encoded.

Categories where DuckDB tends to lead:

* **B (`COUNT(DISTINCT`))** — DuckDB's specialised distinct-aggregator is
  usually 1.5–2× ahead until DataFusion gets the same fast path.
* **E Q23 (`SELECT *` + `ORDER BY` + `LIMIT`)** — DuckDB's TopN is more cache
  friendly for wide projections.

If OpenSnow is 3× or more slower than DuckDB on any single query, that's a
red flag — usually missing predicate pushdown or a regression in the
DataFusion version pinned in `Cargo.toml`.

## How to run

```bash
# Full 100 M-row dataset (downloads 14.8 GB on first run)
bash bench/clickbench.sh

# 100 MB sample for smoke tests in CI
bash bench/clickbench.sh --sf small

# Skip the download if the file is already cached
bash bench/clickbench.sh --skip-download
```

Results are written to `bench/results/clickbench_<timestamp>.csv` with one
row per query: `query_num, opensnow_ms_cold, opensnow_ms_warm,
duckdb_ms_cold, duckdb_ms_warm`.

## In-process microbenchmarks

For tighter feedback than the full ClickBench run, the workspace ships a
`cargo bench` target backed by `criterion` that exercises the three query
shapes ClickBench leans on:

```bash
cargo bench -p opensnow-bench
```

This generates a 1 M-row in-memory table and measures `COUNT(*)`, a
`GROUP BY` aggregation, and a filter+projection on the same data. Use it
to catch local-CPU regressions without paying the full 100 M-row download.
On a recent laptop expect numbers in the 1–10 ms range for `COUNT(*)`,
3–15 ms for `GROUP BY` on 8 categories, and 2–8 ms for the filter
+ projection.
