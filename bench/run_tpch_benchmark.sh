#!/usr/bin/env python3
"""
TPC-H Benchmark: OpenSnow vs DuckDB
Usage: python3 bench/run_tpch_benchmark.sh [--sf 0.1] [--runs 3] [--queries 1,3,6]
"""
import subprocess
import json
import time
import sys
import os
import csv
from datetime import datetime

# Config
SF = 0.1
RUNS = 3
DATA_DIR = "/tmp/opensnow-tpch-bench"
OPENSNOW_URL = "http://localhost:8080"
DUCKDB_DB = "/tmp/opensnow-bench-duckdb.db"
RESULTS_DIR = "/tmp/opensnow-bench-results"
QUERY_FILTER = None

# Parse args
args = sys.argv[1:]
i = 0
while i < len(args):
    if args[i] == "--sf":
        SF = float(args[i+1]); i += 2
    elif args[i] == "--runs":
        RUNS = int(args[i+1]); i += 2
    elif args[i] == "--queries":
        QUERY_FILTER = [int(x) for x in args[i+1].split(",")]; i += 2
    else:
        i += 1

# TPC-H queries (subset that works well on both engines)
QUERIES = {
    1: ("Q1: Pricing Summary",
        "SELECT l_returnflag, l_linestatus, SUM(l_quantity) AS sum_qty, SUM(l_extendedprice) AS sum_base_price, SUM(l_extendedprice * (1.0 - l_discount)) AS sum_disc_price, SUM(l_extendedprice * (1.0 - l_discount) * (1.0 + l_tax)) AS sum_charge, AVG(l_quantity) AS avg_qty, AVG(l_extendedprice) AS avg_price, AVG(l_discount) AS avg_disc, COUNT(*) AS count_order FROM lineitem WHERE l_shipdate <= '1998-09-02' GROUP BY l_returnflag, l_linestatus ORDER BY l_returnflag, l_linestatus"),
    3: ("Q3: Shipping Priority",
        "SELECT l_orderkey, SUM(l_extendedprice * (1.0 - l_discount)) AS revenue, o_orderdate, o_shippriority FROM customer, orders, lineitem WHERE c_mktsegment = 'BUILDING' AND c_custkey = o_custkey AND l_orderkey = o_orderkey AND o_orderdate < '1995-03-15' AND l_shipdate > '1995-03-15' GROUP BY l_orderkey, o_orderdate, o_shippriority ORDER BY revenue DESC, o_orderdate LIMIT 10"),
    4: ("Q4: Order Priority",
        "SELECT o_orderpriority, COUNT(*) AS order_count FROM orders WHERE o_orderdate >= '1993-07-01' AND o_orderdate < '1993-10-01' AND EXISTS (SELECT * FROM lineitem WHERE l_orderkey = o_orderkey AND l_commitdate < l_receiptdate) GROUP BY o_orderpriority ORDER BY o_orderpriority"),
    5: ("Q5: Local Supplier Vol",
        "SELECT n_name, SUM(l_extendedprice * (1.0 - l_discount)) AS revenue FROM customer, orders, lineitem, supplier, nation, region WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey AND l_suppkey = s_suppkey AND c_nationkey = s_nationkey AND s_nationkey = n_nationkey AND n_regionkey = r_regionkey AND r_name = 'ASIA' AND o_orderdate >= '1994-01-01' AND o_orderdate < '1995-01-01' GROUP BY n_name ORDER BY revenue DESC"),
    6: ("Q6: Revenue Forecast",
        "SELECT SUM(l_extendedprice * l_discount) AS revenue FROM lineitem WHERE l_shipdate >= '1994-01-01' AND l_shipdate < '1995-01-01' AND l_discount BETWEEN 0.05 AND 0.07 AND l_quantity < 24.0"),
    10: ("Q10: Returned Items",
         "SELECT c_custkey, c_name, SUM(l_extendedprice * (1.0 - l_discount)) AS revenue, c_acctbal, n_name, c_address, c_phone, c_comment FROM customer, orders, lineitem, nation WHERE c_custkey = o_custkey AND l_orderkey = o_orderkey AND o_orderdate >= '1993-10-01' AND o_orderdate < '1994-01-01' AND l_returnflag = 'R' AND c_nationkey = n_nationkey GROUP BY c_custkey, c_name, c_acctbal, c_phone, n_name, c_address, c_comment ORDER BY revenue DESC LIMIT 20"),
    12: ("Q12: Shipping Modes",
         "SELECT l_shipmode, SUM(CASE WHEN o_orderpriority = '1-URGENT' OR o_orderpriority = '2-HIGH' THEN 1 ELSE 0 END) AS high_line_count, SUM(CASE WHEN o_orderpriority <> '1-URGENT' AND o_orderpriority <> '2-HIGH' THEN 1 ELSE 0 END) AS low_line_count FROM orders, lineitem WHERE o_orderkey = l_orderkey AND l_shipmode IN ('MAIL', 'SHIP') AND l_commitdate < l_receiptdate AND l_shipdate < l_commitdate AND l_receiptdate >= '1994-01-01' AND l_receiptdate < '1995-01-01' GROUP BY l_shipmode ORDER BY l_shipmode"),
    13: ("Q13: Customer Dist",
         "SELECT c_count, COUNT(*) AS custdist FROM (SELECT c_custkey, COUNT(o_orderkey) AS c_count FROM customer LEFT OUTER JOIN orders ON c_custkey = o_custkey AND o_comment NOT LIKE '%special%requests%' GROUP BY c_custkey) AS c_orders GROUP BY c_count ORDER BY custdist DESC, c_count DESC"),
    14: ("Q14: Promo Effect",
         "SELECT 100.00 * SUM(CASE WHEN p_type LIKE 'PROMO%' THEN l_extendedprice * (1.0 - l_discount) ELSE 0.0 END) / SUM(l_extendedprice * (1.0 - l_discount)) AS promo_revenue FROM lineitem, part WHERE l_partkey = p_partkey AND l_shipdate >= '1995-09-01' AND l_shipdate < '1995-10-01'"),
    18: ("Q18: Large Volume Cust",
         "SELECT c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice, SUM(l_quantity) FROM customer, orders, lineitem WHERE o_orderkey IN (SELECT l_orderkey FROM lineitem GROUP BY l_orderkey HAVING SUM(l_quantity) > 300.0) AND c_custkey = o_custkey AND o_orderkey = l_orderkey GROUP BY c_name, c_custkey, o_orderkey, o_orderdate, o_totalprice ORDER BY o_totalprice DESC, o_orderdate LIMIT 100"),
}

active_queries = QUERY_FILTER if QUERY_FILTER else sorted(QUERIES.keys())

def run_cmd(cmd, timeout=120):
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, timeout=timeout)
        return r.returncode, r.stdout, r.stderr
    except subprocess.TimeoutExpired:
        return -1, "", "TIMEOUT"

def run_opensnow(sql):
    import urllib.request
    start = time.perf_counter()
    try:
        req = urllib.request.Request(
            f"{OPENSNOW_URL}/api/v1/query",
            data=json.dumps({"sql": sql}).encode(),
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        with urllib.request.urlopen(req, timeout=120) as resp:
            body = json.loads(resp.read())
        elapsed = (time.perf_counter() - start) * 1000
        if "error" in body and body["error"]:
            return False, elapsed, str(body.get("error", ""))
        return True, elapsed, ""
    except Exception as e:
        elapsed = (time.perf_counter() - start) * 1000
        return False, elapsed, str(e)

def run_duckdb(sql):
    start = time.perf_counter()
    rc, out, err = run_cmd(f'duckdb "{DUCKDB_DB}" -c "{sql}"')
    elapsed = (time.perf_counter() - start) * 1000
    return rc == 0, elapsed, err.strip()

# ==========================================================================
BOLD = "\033[1m"
CYAN = "\033[0;36m"
GREEN = "\033[0;32m"
RED = "\033[0;31m"
YELLOW = "\033[1;33m"
NC = "\033[0m"

print()
print(f"{BOLD}{'='*72}{NC}")
print(f"{BOLD}  TPC-H Benchmark: OpenSnow vs DuckDB{NC}")
print(f"{BOLD}{'='*72}{NC}")
print(f"  Scale Factor:  {CYAN}{SF}{NC}")
print(f"  Bench Runs:    {CYAN}{RUNS}{NC}")
print(f"  Queries:       {CYAN}{', '.join(f'Q{q}' for q in active_queries)}{NC}")
print()

# ---- Step 1: Generate data ----
print(f"{CYAN}[1/4] Generating TPC-H data (SF={SF})...{NC}")
os.makedirs(DATA_DIR, exist_ok=True)

gen_sql = f"""
INSTALL tpch; LOAD tpch; CALL dbgen(sf={SF});
"""
for t in ["region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem"]:
    gen_sql += f"COPY {t} TO '{DATA_DIR}/{t}.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);\n"

rc, out, err = run_cmd(f'duckdb -c "{gen_sql}"', timeout=300)
if rc != 0:
    print(f"{RED}  Failed to generate data: {err}{NC}")
    sys.exit(1)

print(f"  {GREEN}Done.{NC}")
print()

# Show table sizes
print("  Table sizes:")
for t in ["region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem"]:
    path = f"{DATA_DIR}/{t}.parquet"
    size = os.path.getsize(path)
    if size > 1_000_000:
        size_str = f"{size/1_000_000:.1f}M"
    else:
        size_str = f"{size/1_000:.1f}K"
    rc, out, _ = run_cmd(f"duckdb -noheader -csv -c \"SELECT COUNT(*) FROM read_parquet('{path}')\"")
    rows = out.strip()
    print(f"    {t:<12} {size_str:>8}  {rows:>10} rows")
print()

# ---- Step 2: Register in OpenSnow ----
print(f"{CYAN}[2/4] Registering TPC-H tables in OpenSnow...{NC}")

# Check health
ok, _, _ = run_opensnow("SELECT 1")
if not ok:
    # Try health endpoint
    try:
        import urllib.request
        urllib.request.urlopen(f"{OPENSNOW_URL}/health", timeout=5)
    except Exception:
        print(f"{RED}  ERROR: OpenSnow not running at {OPENSNOW_URL}{NC}")
        sys.exit(1)

for t in ["region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem"]:
    path = f"{DATA_DIR}/{t}.parquet"
    sql = f"CREATE EXTERNAL TABLE IF NOT EXISTS {t} STORED AS PARQUET LOCATION '{path}'"
    ok, _, err = run_opensnow(sql)
    if not ok:
        # Try DROP + CREATE
        run_opensnow(f"DROP TABLE IF EXISTS {t}")
        ok, _, err = run_opensnow(sql)
    status = f"{GREEN}OK{NC}" if ok else f"{RED}FAIL: {err}{NC}"
    print(f"    {t}: {status}")
print()

# ---- Step 3: Pre-load DuckDB ----
print(f"{CYAN}[3/4] Pre-loading DuckDB...{NC}")
if os.path.exists(DUCKDB_DB):
    os.remove(DUCKDB_DB)

load_sql = ""
for t in ["region", "nation", "supplier", "customer", "part", "partsupp", "orders", "lineitem"]:
    load_sql += f"CREATE TABLE {t} AS SELECT * FROM read_parquet('{DATA_DIR}/{t}.parquet');\n"
rc, _, err = run_cmd(f'duckdb "{DUCKDB_DB}" -c "{load_sql}"', timeout=120)
print(f"  {GREEN}Done.{NC}")
print()

# ---- Step 4: Run benchmark ----
print(f"{CYAN}[4/4] Running benchmarks...{NC}")
print()

# Warmup
print(f"  {YELLOW}Warming up (1 run each)...{NC}")
for q in active_queries:
    _, sql = QUERIES[q]
    run_opensnow(sql)
    run_duckdb(sql)
print()

results = []

for q in active_queries:
    name, sql = QUERIES[q]
    os_times = []
    dk_times = []
    os_ok = True
    dk_ok = True

    for _ in range(RUNS):
        ok, ms, err = run_opensnow(sql)
        os_times.append(ms)
        if not ok:
            os_ok = False

        ok, ms, err = run_duckdb(sql)
        dk_times.append(ms)
        if not ok:
            dk_ok = False

    os_avg = sum(os_times) / len(os_times)
    dk_avg = sum(dk_times) / len(dk_times)
    results.append((q, name, os_avg, dk_avg, os_ok, dk_ok))

# ---- Print results ----
print()
print(f"{BOLD}{'='*75}{NC}")
print(f"{BOLD}  TPC-H Benchmark Results - Scale Factor {SF}{NC}")
print(f"{BOLD}  OpenSnow (DataFusion) vs DuckDB v1.5  |  {RUNS} runs averaged{NC}")
print(f"{BOLD}{'='*75}{NC}")
print(f"{BOLD}{'Query':<24} {'OpenSnow(ms)':>13} {'DuckDB(ms)':>13} {'Ratio':>13} {'Winner':>10}{NC}")
print(f"{'-'*75}")

os_total = 0
dk_total = 0
os_wins = 0
dk_wins = 0

for q, name, os_ms, dk_ms, os_ok, dk_ok in results:
    os_str = f"{os_ms:.1f}" if os_ok else "FAIL"
    dk_str = f"{dk_ms:.1f}" if dk_ok else "FAIL"

    if os_ok and dk_ok and dk_ms > 0:
        ratio = os_ms / dk_ms
        ratio_str = f"{ratio:.2f}x"
        if os_ms <= dk_ms:
            winner = f"{GREEN}OpenSnow{NC}"
            os_wins += 1
        else:
            winner = f"{RED}DuckDB{NC}"
            dk_wins += 1
        os_total += os_ms
        dk_total += dk_ms
    else:
        ratio_str = "N/A"
        winner = f"{YELLOW}---{NC}"

    print(f"  {name:<22} {os_str:>13} {dk_str:>13} {ratio_str:>13} {winner:>19}")

print(f"{'-'*75}")
if dk_total > 0:
    total_ratio = f"{os_total/dk_total:.2f}x"
else:
    total_ratio = "N/A"
print(f"{BOLD}  {'TOTAL':<22} {os_total:>13.1f} {dk_total:>13.1f} {total_ratio:>13}{NC}")
print(f"{'='*75}")
print()
print(f"  OpenSnow wins: {GREEN}{os_wins}{NC}  |  DuckDB wins: {RED}{dk_wins}{NC}")
print()

# Note about comparison fairness
print(f"  {YELLOW}Note:{NC} OpenSnow times include HTTP overhead (network round-trip).")
print(f"  DuckDB runs as a local process. For fairer comparison, subtract ~2-5ms")
print(f"  from OpenSnow times for network overhead.")
print()

# Save CSV
os.makedirs(RESULTS_DIR, exist_ok=True)
csv_path = f"{RESULTS_DIR}/tpch_sf{SF}_{datetime.now().strftime('%Y%m%d_%H%M%S')}.csv"
with open(csv_path, "w", newline="") as f:
    w = csv.writer(f)
    w.writerow(["query", "name", "opensnow_ms", "duckdb_ms", "ratio", "opensnow_ok", "duckdb_ok"])
    for q, name, os_ms, dk_ms, os_ok, dk_ok in results:
        ratio = f"{os_ms/dk_ms:.4f}" if dk_ok and dk_ms > 0 else "N/A"
        w.writerow([f"Q{q}", name, f"{os_ms:.2f}", f"{dk_ms:.2f}", ratio, os_ok, dk_ok])

print(f"  Results saved to: {CYAN}{csv_path}{NC}")
print()
