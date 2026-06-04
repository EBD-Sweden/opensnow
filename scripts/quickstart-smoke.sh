#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/quickstart-smoke.sh --mode local|docker|k3d

Modes:
  local   Build/run OpenSnow from this checkout, initialize sample data, run
          first shell query, HTTP health/status, REST query, and pgwire query.
  docker  Build/start the Docker Compose demo, then run HTTP smoke checks.
  k3d     Create the k3d cluster from deploy/k3d-config.yaml, install the Helm
          chart with dev values, and run HTTP smoke checks through port-forward.
EOF
}

MODE="local"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$MODE" in
  local|docker|k3d) ;;
  *)
    echo "invalid --mode '$MODE'; expected local, docker, or k3d" >&2
    usage >&2
    exit 2
    ;;
esac

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 127
  fi
}

wait_for_http() {
  local url="$1"
  local attempts="${2:-60}"
  for _ in $(seq 1 "$attempts"); do
    if curl -fsS --max-time 3 "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for $url" >&2
  return 1
}

json_query() {
  local url="$1"
  local sql="$2"
  python3 - "$sql" <<'PY' | curl -fsS --max-time 15 -H 'content-type: application/json' -d @- "$url"
import json, sys
print(json.dumps({"sql": sys.argv[1]}))
PY
}

run_http_smoke() {
  local base_url="${1:-http://127.0.0.1:8080}"
  require curl
  require python3

  echo "==> HTTP health: $base_url/health"
  curl -fsS --max-time 5 "$base_url/health" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "ok", d; print(d)'

  echo "==> HTTP status: $base_url/api/v1/status"
  curl -fsS --max-time 5 "$base_url/api/v1/status" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "running", d; print(d)'

  echo "==> REST query: cdrs grouped by call_type"
  json_query "$base_url/api/v1/query" "SELECT call_type, COUNT(*) AS n FROM cdrs GROUP BY call_type ORDER BY call_type" \
    | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "ok", d; assert d.get("rows") == 3, d; print(d)'
}

run_local() {
  require cargo
  require curl
  require python3
  require psql

  echo "==> Build local opensnow binary"
  OPENSNOW_OTEL_DISABLED=1 cargo build --quiet --bin opensnow
  local opensnow_bin="${OPENSNOW_BIN:-target/debug/opensnow}"

  local tmp
  tmp="$(mktemp -d -t opensnow-quickstart-smoke.XXXXXX)"
  SMOKE_TMP="$tmp"
  SMOKE_SERVER_PID=""
  cleanup() {
    if [ -n "${SMOKE_SERVER_PID:-}" ] && kill -0 "$SMOKE_SERVER_PID" >/dev/null 2>&1; then
      kill "$SMOKE_SERVER_PID" >/dev/null 2>&1 || true
      wait "$SMOKE_SERVER_PID" >/dev/null 2>&1 || true
    fi
    rm -rf "${SMOKE_TMP:-}"
  }
  trap cleanup EXIT

  local http_port pg_port
  http_port=$((18080 + ($$ % 1000)))
  pg_port=$((15433 + ($$ % 1000)))

  cat >"$tmp/opensnow.toml" <<EOF
[server]
http_port = $http_port
pg_port = $pg_port
pg_enabled = true
host = "127.0.0.1"

[storage]
warehouse_path = "$tmp/warehouse"

[catalog]
path = "$tmp/catalog.db"
EOF

  echo "==> Local init sample data"
  OPENSNOW_OTEL_DISABLED=1 "$opensnow_bin" \
    --config "$tmp/opensnow.toml" \
    init --with-sample-data --industry both

  echo "==> First shell query"
  OPENSNOW_OTEL_DISABLED=1 "$opensnow_bin" \
    --config "$tmp/opensnow.toml" \
    shell -c "SELECT call_type, COUNT(*) AS n FROM cdrs GROUP BY call_type ORDER BY call_type"

  echo "==> Start local server on 127.0.0.1:$http_port and pgwire 127.0.0.1:$pg_port"
  OPENSNOW_OTEL_DISABLED=1 "$opensnow_bin" \
    --config "$tmp/opensnow.toml" \
    start --enable-pgwire >"$tmp/server.log" 2>&1 &
  SMOKE_SERVER_PID="$!"

  wait_for_http "http://127.0.0.1:$http_port/health" 90
  run_http_smoke "http://127.0.0.1:$http_port"

  echo "==> pgwire query via psql"
  psql -h 127.0.0.1 -p "$pg_port" -U admin -d opensnow \
    -c 'SELECT COUNT(*) AS n FROM cdrs;'
}

run_docker() {
  require docker
  require curl
  require python3

  echo "==> Docker Compose rendered config"
  docker compose config >/dev/null

  echo "==> Docker Compose sample data init"
  docker compose run --rm opensnow init --with-sample-data --industry both

  echo "==> Docker Compose quickstart"
  docker compose up -d --build opensnow
  trap 'docker compose down --remove-orphans' EXIT
  wait_for_http "http://127.0.0.1:8080/health" 120
  run_http_smoke "http://127.0.0.1:8080"
}

run_k3d() {
  require k3d
  require kubectl
  require helm
  require curl
  require python3

  echo "==> k3d cluster create --config deploy/k3d-config.yaml"
  k3d cluster create --config deploy/k3d-config.yaml

  echo "==> Helm install with chart-consumed dev values"
  helm upgrade --install opensnow deploy/helm/opensnow \
    -f deploy/helm/opensnow/values-dev.yaml \
    --set config.storage.type=s3 \
    --set config.storage.endpoint=http://opensnow-minio:9000 \
    --set worker.replicas=3

  kubectl rollout status deploy/opensnow-coordinator --timeout=180s
  kubectl port-forward svc/opensnow-gateway 8080:8080 >/tmp/opensnow-k3d-port-forward.log 2>&1 &
  local pf_pid="$!"
  trap 'kill "$pf_pid" >/dev/null 2>&1 || true' EXIT
  wait_for_http "http://127.0.0.1:8080/health" 60
  run_http_smoke "http://127.0.0.1:8080"
}

case "$MODE" in
  local) run_local ;;
  docker) run_docker ;;
  k3d) run_k3d ;;
esac

echo "OpenSnow quickstart smoke passed for mode: $MODE"
