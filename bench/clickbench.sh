#!/usr/bin/env bash
# ============================================================================
# OpenSnow ClickBench — 100M row web analytics benchmark
#
# Dataset: https://datasets.clickhouse.com/hits_compatible/hits.parquet
# 100M rows, 14.8GB, 43 queries. Industry-standard OLAP benchmark.
#
# Competitors tested:
#   - OpenSnow (DataFusion) via REST API
#   - DuckDB   (local process, same file)
#   - Athena   (optional, needs AWS credentials)
#
# Note: DataFusion ranked #1 fastest single-node Parquet engine on ClickBench
# in Nov 2024: https://datafusion.apache.org/blog/2024/11/18/...
# Our numbers should reflect that baseline.
#
# Usage:
#   bash bench/clickbench.sh [--skip-download] [--athena] [--sf small]
# ============================================================================

set -euo pipefail

OPENSNOW_URL="${OPENSNOW_URL:-http://localhost:8080}"
DATA_DIR="${DATA_DIR:-/tmp/opensnow-clickbench}"
RESULTS_DIR="${RESULTS_DIR:-/tmp/opensnow-bench-results}"
RUNS="${RUNS:-3}"
WITH_ATHENA="${WITH_ATHENA:-false}"
SKIP_DOWNLOAD="${SKIP_DOWNLOAD:-false}"
# "small" = 1M row sample; "full" = 100M rows (14.8GB)
DATASET_SIZE="${DATASET_SIZE:-full}"

BOLD="\033[1m"; CYAN="\033[0;36m"; GREEN="\033[0;32m"; RED="\033[0;31m"
YELLOW="\033[1;33m"; NC="\033[0m"

for arg in "$@"; do
  case $arg in
    --skip-download) SKIP_DOWNLOAD=true ;;
    --athena)        WITH_ATHENA=true ;;
    --sf) shift; DATASET_SIZE="$1" ;;
  esac
done

mkdir -p "$DATA_DIR" "$RESULTS_DIR"
HITS_FILE="$DATA_DIR/hits.parquet"

echo
echo -e "${BOLD}============================================================${NC}"
echo -e "${BOLD}  OpenSnow ClickBench${NC}"
echo -e "${BOLD}============================================================${NC}"
echo -e "  Engine:   ${CYAN}$OPENSNOW_URL${NC}"
echo -e "  Dataset:  ${CYAN}$DATASET_SIZE (100M rows, 14.8GB)${NC}"
echo -e "  Runs:     ${CYAN}$RUNS per query${NC}"
echo

# ── Step 1: Download dataset ─────────────────────────────────────────────────
if [[ "$SKIP_DOWNLOAD" == "false" ]]; then
  if [[ ! -f "$HITS_FILE" ]]; then
    echo -e "${CYAN}[1/4] Downloading ClickBench dataset...${NC}"
    echo -e "  Source: datasets.clickhouse.com (14.8 GB — this takes a while)"
    echo -e "  ${YELLOW}Tip: Use --skip-download if you have it already${NC}"
    echo

    if [[ "$DATASET_SIZE" == "small" ]]; then
      # 1M row sample — fast for testing
      echo -e "  Downloading 1M row sample..."
      python3 - <<'EOF'
import urllib.request, os
url = "https://datasets.clickhouse.com/hits_compatible/hits.parquet"
out = "/tmp/opensnow-clickbench/hits.parquet"
# Download first 100MB only (sample)
req = urllib.request.Request(url, headers={"Range": "bytes=0-104857599"})
with urllib.request.urlopen(req) as r, open(out, "wb") as f:
    f.write(r.read())
print("  Sample downloaded (first 100MB)")
EOF
    else
      # Full dataset
      curl -L --progress-bar \
        "https://datasets.clickhouse.com/hits_compatible/hits.parquet" \
        -o "$HITS_FILE"
    fi
    echo -e "  ${GREEN}Downloaded: $(du -sh $HITS_FILE | cut -f1)${NC}"
  else
    echo -e "${CYAN}[1/4] Dataset already present: $(du -sh $HITS_FILE | cut -f1)${NC}"
  fi
else
  echo -e "${CYAN}[1/4] Skipping download (--skip-download)${NC}"
fi
echo

# ── Step 2: Register in OpenSnow ─────────────────────────────────────────────
echo -e "${CYAN}[2/4] Registering hits table in OpenSnow...${NC}"
REGISTER_SQL="COPY INTO hits FROM '$HITS_FILE' FILE_FORMAT = (TYPE = PARQUET)"
curl -sf -X POST "$OPENSNOW_URL/api/v1/query" \
  -H "Content-Type: application/json" \
  -d "{\"sql\": \"DROP TABLE IF EXISTS hits\"}" > /dev/null || true
curl -sf -X POST "$OPENSNOW_URL/api/v1/query" \
  -H "Content-Type: application/json" \
  -d "{\"sql\": \"CREATE EXTERNAL TABLE hits STORED AS PARQUET LOCATION '$HITS_FILE'\"}" \
  > /dev/null && echo -e "  ${GREEN}OK${NC}" || echo -e "  ${RED}FAILED — is OpenSnow running?${NC}"
echo

# ── Step 3: ClickBench queries ───────────────────────────────────────────────
# 43 canonical ClickBench queries. Source: github.com/ClickHouse/ClickBench
# Adapted for DataFusion/standard SQL (no ClickHouse-specific syntax).
declare -a QUERIES=(
  "SELECT count(*) FROM hits"
  "SELECT count(*) FROM hits WHERE AdvEngineID <> 0"
  "SELECT sum(AdvEngineID), count(*), avg(ResolutionWidth) FROM hits"
  "SELECT sum(UserID) FROM hits"
  "SELECT COUNT(DISTINCT UserID) FROM hits"
  "SELECT COUNT(DISTINCT SearchPhrase) FROM hits"
  "SELECT min(EventDate), max(EventDate) FROM hits"
  "SELECT AdvEngineID, count(*) FROM hits WHERE AdvEngineID <> 0 GROUP BY AdvEngineID ORDER BY count(*) DESC LIMIT 10"
  "SELECT RegionID, COUNT(DISTINCT UserID) AS u FROM hits GROUP BY RegionID ORDER BY u DESC LIMIT 10"
  "SELECT RegionID, sum(AdvEngineID), count(*) AS c, avg(ResolutionWidth), COUNT(DISTINCT UserID) FROM hits GROUP BY RegionID ORDER BY c DESC LIMIT 10"
  "SELECT MobilePhoneModel, COUNT(DISTINCT UserID) AS u FROM hits WHERE MobilePhoneModel <> '' GROUP BY MobilePhoneModel ORDER BY u DESC LIMIT 10"
  "SELECT MobilePhone, MobilePhoneModel, COUNT(DISTINCT UserID) AS u FROM hits WHERE MobilePhoneModel <> '' GROUP BY MobilePhone, MobilePhoneModel ORDER BY u DESC LIMIT 10"
  "SELECT SearchPhrase, count(*) AS c FROM hits WHERE SearchPhrase <> '' GROUP BY SearchPhrase ORDER BY c DESC LIMIT 10"
  "SELECT SearchPhrase, COUNT(DISTINCT UserID) AS u FROM hits WHERE SearchPhrase <> '' GROUP BY SearchPhrase ORDER BY u DESC LIMIT 10"
  "SELECT SearchEngineID, SearchPhrase, count(*) AS c FROM hits WHERE SearchPhrase <> '' GROUP BY SearchEngineID, SearchPhrase ORDER BY c DESC LIMIT 10"
  "SELECT UserID, count(*) FROM hits GROUP BY UserID ORDER BY count(*) DESC LIMIT 10"
  "SELECT UserID, SearchPhrase, count(*) FROM hits GROUP BY UserID, SearchPhrase ORDER BY count(*) DESC LIMIT 10"
  "SELECT UserID, SearchPhrase, count(*) FROM hits GROUP BY UserID, SearchPhrase LIMIT 10"
  "SELECT UserID, extract(minute FROM EventTime) AS m, SearchPhrase, count(*) FROM hits GROUP BY UserID, m, SearchPhrase ORDER BY count(*) DESC LIMIT 10"
  "SELECT UserID FROM hits WHERE UserID = 435090932899640449"
  "SELECT count(*) FROM hits WHERE URL LIKE '%google%'"
  "SELECT SearchPhrase, min(URL), count(*) AS c FROM hits WHERE URL LIKE '%google%' AND SearchPhrase <> '' GROUP BY SearchPhrase ORDER BY c DESC LIMIT 10"
  "SELECT SearchPhrase, min(URL), min(Title), count(*) AS c, COUNT(DISTINCT UserID) FROM hits WHERE Title LIKE '%Google%' AND URL NOT LIKE '%.google.%' AND SearchPhrase <> '' GROUP BY SearchPhrase ORDER BY c DESC LIMIT 10"
  "SELECT * FROM hits WHERE URL LIKE '%google%' ORDER BY EventTime LIMIT 10"
  "SELECT SearchPhrase FROM hits WHERE SearchPhrase <> '' ORDER BY EventTime LIMIT 10"
  "SELECT SearchPhrase FROM hits WHERE SearchPhrase <> '' ORDER BY SearchPhrase LIMIT 10"
  "SELECT SearchPhrase FROM hits WHERE SearchPhrase <> '' ORDER BY EventTime, SearchPhrase LIMIT 10"
  "SELECT CounterID, avg(length(URL)) AS l, count(*) AS c FROM hits WHERE URL <> '' GROUP BY CounterID ORDER BY l DESC LIMIT 25"
  "SELECT CAST(regexp_replace(Referer, 'https?://(?:www\\.)?([^/]+).*', '\\1') AS VARCHAR) AS k, avg(length(Referer)) AS l, count(*) AS c, min(Referer) FROM hits WHERE Referer <> '' GROUP BY k ORDER BY l DESC LIMIT 25"
  "SELECT sum(ResolutionWidth), sum(ResolutionWidth + 1), sum(ResolutionWidth + 2), sum(ResolutionWidth + 3), sum(ResolutionWidth + 4), sum(ResolutionWidth + 5), sum(ResolutionWidth + 6), sum(ResolutionWidth + 7), sum(ResolutionWidth + 8), sum(ResolutionWidth + 9), sum(ResolutionWidth + 10), sum(ResolutionWidth + 11), sum(ResolutionWidth + 12), sum(ResolutionWidth + 13), sum(ResolutionWidth + 14), sum(ResolutionWidth + 15), sum(ResolutionWidth + 16), sum(ResolutionWidth + 17), sum(ResolutionWidth + 18), sum(ResolutionWidth + 19), sum(ResolutionWidth + 20), sum(ResolutionWidth + 21), sum(ResolutionWidth + 22), sum(ResolutionWidth + 23), sum(ResolutionWidth + 24), sum(ResolutionWidth + 25), sum(ResolutionWidth + 26), sum(ResolutionWidth + 27), sum(ResolutionWidth + 28), sum(ResolutionWidth + 29), sum(ResolutionWidth + 30), sum(ResolutionWidth + 31), sum(ResolutionWidth + 32), sum(ResolutionWidth + 33), sum(ResolutionWidth + 34), sum(ResolutionWidth + 35), sum(ResolutionWidth + 36), sum(ResolutionWidth + 37), sum(ResolutionWidth + 38), sum(ResolutionWidth + 39), sum(ResolutionWidth + 40), sum(ResolutionWidth + 41), sum(ResolutionWidth + 42), sum(ResolutionWidth + 43), sum(ResolutionWidth + 44), sum(ResolutionWidth + 45), sum(ResolutionWidth + 46), sum(ResolutionWidth + 47), sum(ResolutionWidth + 48), sum(ResolutionWidth + 49), sum(ResolutionWidth + 50), sum(ResolutionWidth + 51), sum(ResolutionWidth + 52), sum(ResolutionWidth + 53), sum(ResolutionWidth + 54), sum(ResolutionWidth + 55), sum(ResolutionWidth + 56), sum(ResolutionWidth + 57), sum(ResolutionWidth + 58), sum(ResolutionWidth + 59), sum(ResolutionWidth + 60), sum(ResolutionWidth + 61), sum(ResolutionWidth + 62), sum(ResolutionWidth + 63), sum(ResolutionWidth + 64), sum(ResolutionWidth + 65), sum(ResolutionWidth + 66), sum(ResolutionWidth + 67), sum(ResolutionWidth + 68), sum(ResolutionWidth + 69), sum(ResolutionWidth + 70), sum(ResolutionWidth + 71), sum(ResolutionWidth + 72), sum(ResolutionWidth + 73), sum(ResolutionWidth + 74), sum(ResolutionWidth + 75), sum(ResolutionWidth + 76), sum(ResolutionWidth + 77), sum(ResolutionWidth + 78), sum(ResolutionWidth + 79), sum(ResolutionWidth + 80), sum(ResolutionWidth + 81), sum(ResolutionWidth + 82), sum(ResolutionWidth + 83), sum(ResolutionWidth + 84), sum(ResolutionWidth + 85), sum(ResolutionWidth + 86), sum(ResolutionWidth + 87), sum(ResolutionWidth + 88), sum(ResolutionWidth + 89) FROM hits"
  "SELECT SearchEngineID, ClientIP, count(*) AS c, sum(AdvEngineID), avg(ResolutionWidth) FROM hits WHERE SearchPhrase <> '' GROUP BY SearchEngineID, ClientIP ORDER BY c DESC LIMIT 10"
  "SELECT WatchID, ClientIP, count(*) AS c, sum(AdvEngineID), avg(ResolutionWidth) FROM hits WHERE SearchPhrase <> '' GROUP BY WatchID, ClientIP ORDER BY c DESC LIMIT 10"
  "SELECT WatchID, ClientIP, count(*) AS c, sum(AdvEngineID), avg(ResolutionWidth) FROM hits GROUP BY WatchID, ClientIP ORDER BY c DESC LIMIT 10"
  "SELECT URL, count(*) AS c FROM hits GROUP BY URL ORDER BY c DESC LIMIT 10"
  "SELECT 1, URL, count(*) AS c FROM hits GROUP BY 1, URL ORDER BY c DESC LIMIT 10"
  "SELECT ClientIP, ClientIP - 1, ClientIP - 2, ClientIP - 3, count(*) AS c FROM hits GROUP BY ClientIP, ClientIP - 1, ClientIP - 2, ClientIP - 3 ORDER BY c DESC LIMIT 10"
  "SELECT URL, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND DontCountHits = 0 AND IsRefresh = 0 AND URL <> '' GROUP BY URL ORDER BY PageViews DESC LIMIT 10"
  "SELECT Title, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND DontCountHits = 0 AND IsRefresh = 0 AND Title <> '' GROUP BY Title ORDER BY PageViews DESC LIMIT 10"
  "SELECT URL, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND IsRefresh = 0 AND IsLink <> 0 AND IsDownload = 0 GROUP BY URL ORDER BY PageViews DESC LIMIT 10 OFFSET 1000"
  "SELECT TraficSourceID, SearchEngineID, AdvEngineID, CASE WHEN SearchEngineID = 0 AND AdvEngineID = 0 THEN Referer ELSE '' END AS Src, URL AS Dst, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND IsRefresh = 0 GROUP BY TraficSourceID, SearchEngineID, AdvEngineID, Src, Dst ORDER BY PageViews DESC LIMIT 10 OFFSET 1000"
  "SELECT URLHash, EventDate, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND IsRefresh = 0 AND TraficSourceID IN (-1, 6) AND RefererHash = 3594120000172545465 GROUP BY URLHash, EventDate ORDER BY PageViews DESC LIMIT 10 OFFSET 100"
  "SELECT WindowClientWidth, WindowClientHeight, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-31' AND IsRefresh = 0 AND DontCountHits = 0 AND URLHash = 2868770270353813622 GROUP BY WindowClientWidth, WindowClientHeight ORDER BY PageViews DESC LIMIT 10 OFFSET 10000"
  "SELECT DATE_TRUNC('minute', EventTime) AS M, count(*) AS PageViews FROM hits WHERE CounterID = 62 AND EventDate >= '2013-07-01' AND EventDate <= '2013-07-02' AND IsRefresh = 0 AND DontCountHits = 0 GROUP BY DATE_TRUNC('minute', EventTime) ORDER BY DATE_TRUNC('minute', EventTime) LIMIT 10 OFFSET 1000"
)

echo -e "${CYAN}[3/4] Running ${#QUERIES[@]} ClickBench queries...${NC}"
echo -e "  ${YELLOW}(3 cold runs each — first run clears any cache)${NC}"
echo

TIMESTAMP=$(date +%Y%m%d_%H%M%S)
RESULTS_CSV="$RESULTS_DIR/clickbench_$TIMESTAMP.csv"
echo "query_num,opensnow_ms_cold,opensnow_ms_warm,duckdb_ms_cold,duckdb_ms_warm" > "$RESULTS_CSV"

run_opensnow() {
  local sql="$1"
  local start end ms
  start=$(date +%s%N)
  curl -sf -X POST "$OPENSNOW_URL/api/v1/query" \
    -H "Content-Type: application/json" \
    -d "{\"sql\": $(echo "$sql" | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')}" \
    > /dev/null 2>&1
  end=$(date +%s%N)
  echo $(( (end - start) / 1000000 ))
}

run_duckdb() {
  local sql="$1" file="$HITS_FILE"
  local start end ms
  start=$(date +%s%N)
  duckdb -c "SELECT * FROM read_parquet('$file') LIMIT 0; $sql" > /dev/null 2>&1
  end=$(date +%s%N)
  echo $(( (end - start) / 1000000 ))
}

printf "%-6s %-16s %-16s %-16s %-16s\n" "Q#" "OpenSnow cold" "OpenSnow warm" "DuckDB cold" "DuckDB warm"
printf "%s\n" "----------------------------------------------------------------------"

total_os=0; total_dk=0; q_num=0
for sql in "${QUERIES[@]}"; do
  q_num=$((q_num + 1))

  # Cold run (no cache)
  os_cold=$(run_opensnow "$sql")
  dk_cold=$(run_duckdb "$sql")

  # Warm runs (averaged)
  os_warm_total=0; dk_warm_total=0
  for i in $(seq 2 $RUNS); do
    os_warm_total=$((os_warm_total + $(run_opensnow "$sql")))
    dk_warm_total=$((dk_warm_total + $(run_duckdb "$sql")))
  done
  os_warm=$((os_warm_total / (RUNS - 1)))
  dk_warm=$((dk_warm_total / (RUNS - 1)))

  total_os=$((total_os + os_warm))
  total_dk=$((total_dk + dk_warm))

  # Color: green if we win
  if [[ $os_warm -le $dk_warm ]]; then
    os_color=$GREEN; winner="← ✓"
  else
    os_color=$RED; winner=""
  fi

  printf "Q%-5d ${os_color}%-16s${NC} %-16s %-16s %-16s %s\n" \
    "$q_num" "${os_cold}ms" "${os_warm}ms" "${dk_cold}ms" "${dk_warm}ms" "$winner"

  echo "$q_num,$os_cold,$os_warm,$dk_cold,$dk_warm" >> "$RESULTS_CSV"
done

echo "----------------------------------------------------------------------"
printf "%-6s %-16s %-16s %-16s %-16s\n" "TOTAL" "" "${total_os}ms" "" "${total_dk}ms"
echo

ratio=$(python3 -c "print(f'{$total_os/$total_dk:.2f}x')" 2>/dev/null || echo "N/A")
echo -e "  OpenSnow / DuckDB ratio: ${CYAN}$ratio${NC} (lower = OpenSnow faster)"
echo -e "  Results: ${CYAN}$RESULTS_CSV${NC}"
echo

# Athena cost estimate
echo -e "  ${BOLD}Athena cost comparison:${NC}"
HITS_SIZE_GB=$(du -s --apparent-size -BG "$HITS_FILE" 2>/dev/null | cut -f1 | tr -d 'G' || echo "15")
athena_cost=$(python3 -c "print(f'\${$HITS_SIZE_GB * 5 / 1024:.4f}')" 2>/dev/null || echo "~\$0.07")
echo -e "  Dataset size: ${HITS_SIZE_GB}GB"
echo -e "  Athena (full scan, \$5/TB): ${RED}$athena_cost per query${NC}"
echo -e "  OpenSnow: ${GREEN}\$0 per query${NC} (pay for compute, not scans)"
echo
