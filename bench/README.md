# OpenSnow Benchmark Suite

Three benchmarks, three datasets. Each covers a different angle.

---

## Benchmarks

### 1. TPC-H (built-in, synthetic)
`bench/run_tpch_benchmark.sh`

Standard analytical benchmark — complex JOINs, aggregations, subqueries.
22 queries across 8 tables. We generate the data locally via DuckDB.

Competitors: **DuckDB** (local)

```bash
python3 bench/run_tpch_benchmark.sh --sf 1 --runs 3
```

---

### 2. ClickBench (100M rows, real web analytics data)
`bench/clickbench.sh`

The industry-standard OLAP benchmark from ClickHouse.
100M rows of web analytics events, 43 queries, 14.8GB Parquet.

**Why it matters:** DataFusion (our engine) ranked #1 fastest single-node engine
on ClickBench in Nov 2024. We should document our numbers.

Dataset: public, hosted by ClickHouse — no AWS account needed.

Competitors: **DuckDB**, **Athena** (same S3 file, same SQL)

```bash
bash bench/clickbench.sh
```

---

### 3. NYC Taxi (real S3 dataset, cloud-native)
`bench/nyc_taxi.sh`

Real-world dataset on public S3. Tests actual S3 read performance,
VPC endpoint benefit, and Athena cost comparison.

~3B rows across 2009–2024, partitioned by year/month Parquet on S3.
No download needed — query directly from S3.

Competitors: **Athena** (pay-per-query, $5/TB scanned)

```bash
bash bench/nyc_taxi.sh                 # OpenSnow only
bash bench/nyc_taxi.sh --athena        # also run Athena and compare (needs AWS creds)
bash bench/nyc_taxi.sh --year 2023     # single year (~84M rows)
bash bench/nyc_taxi.sh --years 2019,2020
```

---

## Dataset Reference

| Dataset | Rows | Size | Source | Cost |
|---|---|---|---|---|
| TPC-H SF1 | ~6M lineitem | ~1GB | Generated locally | Free |
| TPC-H SF10 | ~60M lineitem | ~10GB | Generated locally | Free |
| ClickBench hits | 100M | 14.8GB Parquet | datasets.clickhouse.com | Free |
| NYC Taxi (2019) | ~84M | ~1.5GB Parquet | AWS Open Data | Free (public S3) |
| NYC Taxi (all) | ~3B | ~50GB Parquet | AWS Open Data | Free (public S3) |

---

## Competitor cost model (Athena)

Athena charges **$5 per TB scanned** (after compression).

| Query | Data scanned | Athena cost |
|---|---|---|
| ClickBench Q1 (full scan) | 14.8 GB | ~$0.07 |
| NYC Taxi 1 year (1 col) | ~100 MB (col pruning) | ~$0.0005 |
| NYC Taxi 1 year (full) | ~1.5 GB | ~$0.0075 |
| TPC-H SF10 Q1 | ~4 GB lineitem | ~$0.02 |

OpenSnow: **$0 per query** (you pay for compute, not scans).
