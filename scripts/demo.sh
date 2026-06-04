#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_URL="${OPENSNOW_BASE_URL:-http://localhost:8080}"
HTTP_PORT="${OPENSNOW_HTTP_PORT:-8080}"
PG_PORT="${OPENSNOW_PGPORT:-5433}"
OPENSNOW_ENABLE_PGWIRE="${OPENSNOW_ENABLE_PGWIRE:-0}"
DEMO_HOME="${OPENSNOW_DEMO_HOME:-$ROOT/.opensnow-demo}"
CONFIG="$DEMO_HOME/opensnow.toml"
PIDFILE="$DEMO_HOME/opensnow.pid"
LOGFILE="$DEMO_HOME/opensnow.log"
MANIFEST="$ROOT/demo/public-demo-manifest.json"
SEED_SCRIPT="$ROOT/scripts/demo-seed.py"

usage() {
  cat <<'USAGE'
OpenSnow public demo

Usage:
  scripts/demo.sh          Start local demo, load stable sample data, run smoke checks.
  scripts/demo.sh reset    Stop the demo process started by this script and remove demo state.

Environment overrides:
  OPENSNOW_BASE_URL=http://localhost:8080
  OPENSNOW_HTTP_PORT=8080
  OPENSNOW_PGPORT=5433
  OPENSNOW_DEMO_HOME=/absolute/path/to/demo-state
  OPENSNOW_ENABLE_PGWIRE=0 Trusted-local opt-in: pass --enable-pgwire to the server.
  OPENSNOW_SKIP_PG=1       Smoke-only flag: skip client pgwire checks so psql is optional.

Trusted-local pgwire example:
  OPENSNOW_ENABLE_PGWIRE=1 OPENSNOW_SKIP_PG=0 scripts/demo.sh
USAGE
}

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 127
  fi
}

write_config() {
  mkdir -p "$DEMO_HOME/warehouse" "$DEMO_HOME/catalog"
  cat >"$CONFIG" <<EOF_CONFIG
[server]
host = "127.0.0.1"
http_port = $HTTP_PORT
pg_port = $PG_PORT

[storage]
warehouse_path = "$DEMO_HOME/warehouse"

[catalog]
path = "$DEMO_HOME/catalog/catalog.db"
EOF_CONFIG
}

health_ready() {
  curl -fsS "$BASE_URL/health" >/dev/null 2>&1
}

wait_for_health() {
  local attempts="${OPENSNOW_DEMO_HEALTH_ATTEMPTS:-120}"
  for _ in $(seq 1 "$attempts"); do
    if health_ready; then
      return 0
    fi
    sleep 1
  done
  echo "OpenSnow did not become healthy at $BASE_URL/health" >&2
  echo "Server log: $LOGFILE" >&2
  return 1
}

stop_demo_process() {
  if [ -f "$PIDFILE" ]; then
    pid="$(cat "$PIDFILE")"
    if [ -n "$pid" ] && kill -0 "$pid" >/dev/null 2>&1; then
      echo "Stopping OpenSnow demo process $pid"
      kill "$pid" >/dev/null 2>&1 || true
      for _ in $(seq 1 10); do
        if ! kill -0 "$pid" >/dev/null 2>&1; then
          break
        fi
        sleep 1
      done
    fi
  fi
}

start_server() {
  require cargo
  require curl
  require python3
  write_config

  if health_ready && [ "$OPENSNOW_ENABLE_PGWIRE" = "1" ] && [ -f "$PIDFILE" ]; then
    echo "Restarting repo-owned demo server with trusted-local pgwire enabled"
    stop_demo_process
  fi

  if health_ready; then
    echo "OpenSnow is already healthy at $BASE_URL"
    return 0
  fi

  echo "Starting OpenSnow demo server on $BASE_URL (state: $DEMO_HOME)"
  (
    cd "$ROOT"
    server_args=(--config "$CONFIG" start --http-port "$HTTP_PORT" --pg-port "$PG_PORT")
    if [ "$OPENSNOW_ENABLE_PGWIRE" = "1" ]; then
      server_args+=(--enable-pgwire)
    fi
    cargo run -p opensnow-cli -- "${server_args[@]}"
  ) >"$LOGFILE" 2>&1 &
  echo "$!" >"$PIDFILE"
  wait_for_health
}

seed_and_smoke() {
  python3 "$SEED_SCRIPT" --base-url "$BASE_URL" "$MANIFEST"
  OPENSNOW_BASE_URL="$BASE_URL" \
  OPENSNOW_PGPORT="$PG_PORT" \
  OPENSNOW_ENABLE_PGWIRE="${OPENSNOW_ENABLE_PGWIRE:-0}" \
  OPENSNOW_SKIP_PG="${OPENSNOW_SKIP_PG:-1}" \
    "$ROOT/scripts/public-smoke.sh"
}

reset_demo() {
  stop_demo_process

  case "$DEMO_HOME" in
    "$ROOT"/.opensnow-demo|"$ROOT"/.opensnow-demo/*)
      echo "Removing demo state $DEMO_HOME"
      rm -rf -- "$DEMO_HOME"
      ;;
    *)
      echo "Refusing to remove OPENSNOW_DEMO_HOME outside repo-owned .opensnow-demo: $DEMO_HOME" >&2
      echo "Remove it manually if this override is intentional." >&2
      exit 2
      ;;
  esac
}

case "${1:-start}" in
  start|up|run)
    start_server
    seed_and_smoke
    cat <<EOF_NEXT

OpenSnow public demo is ready.

Web UI:        $BASE_URL
Health:        $BASE_URL/health
Manifest:      demo/public-demo-manifest.json
Demo state:    $DEMO_HOME
Server log:    $LOGFILE

Try:
  curl -fsS -H 'content-type: application/json' \\
    -d '{"sql":"SELECT status, COUNT(*) AS orders FROM demo_orders GROUP BY status ORDER BY status"}' \\
    $BASE_URL/api/v1/query

Reset:
  scripts/demo.sh reset
EOF_NEXT
    ;;
  reset)
    reset_demo
    ;;
  clean)
    reset_demo
    ;;
  help|-h|--help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
