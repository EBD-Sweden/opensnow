#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${OPENSNOW_BASE_URL:-http://localhost:8080}"
PGHOST="${OPENSNOW_PGHOST:-localhost}"
PGPORT="${OPENSNOW_PGPORT:-5433}"
PGDATABASE="${OPENSNOW_PGDATABASE:-opensnow}"
PGUSER="${OPENSNOW_PGUSER:-opensnow}"
TABLE="${OPENSNOW_SMOKE_TABLE:-public_smoke_$$}"

require() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 127
  fi
}

require curl
require python3

if ! [[ "$TABLE" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
  echo "invalid OPENSNOW_SMOKE_TABLE: use an unquoted SQL identifier containing only letters, digits, and underscores" >&2
  exit 2
fi

echo "==> HTTP health: $BASE_URL/health"
curl -fsS "$BASE_URL/health" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "ok", d; print(d)'

echo "==> HTTP status: $BASE_URL/api/v1/status"
curl -fsS "$BASE_URL/api/v1/status" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "running", d; print(d)'

echo "==> REST ingest: $TABLE"
python3 - "$TABLE" <<'PY' | curl -fsS -H 'content-type: application/json' -d @- "$BASE_URL/api/v1/ingest" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "ok", d; assert d.get("rows_ingested") == 3, d; print(d)'
import json, sys
print(json.dumps({
    "table": sys.argv[1],
    "columns": ["id", "region", "amount"],
    "rows": [[1, "stockholm", 10.5], [2, "gothenburg", 20.0], [3, "malmo", 7.5]],
    "replace": True,
}))
PY

echo "==> REST query: count $TABLE"
python3 - "$TABLE" <<'PY' | curl -fsS -H 'content-type: application/json' -d @- "$BASE_URL/api/v1/query" \
  | python3 -c 'import json,sys; d=json.load(sys.stdin); assert d.get("status") == "ok", d; assert d.get("rows", 0) >= 1, d; print(d)'
import json, sys
print(json.dumps({"sql": f"SELECT COUNT(*) AS rows FROM {sys.argv[1]}"}))
PY

if [ "${OPENSNOW_ENABLE_PGWIRE:-0}" != "1" ] || [ "${OPENSNOW_SKIP_PG:-0}" = "1" ]; then
  echo "==> Skipping pgwire check; pgwire is disabled by default for public demos"
  echo "    To test trusted local pgwire, start with --enable-pgwire or pg_enabled=true and run OPENSNOW_ENABLE_PGWIRE=1 scripts/public-smoke.sh"
else
  require psql
  echo "==> PostgreSQL wire smoke via psql simple-query: $PGHOST:$PGPORT"
  psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -c "SELECT COUNT(*) AS rows FROM $TABLE;"

  echo "==> PostgreSQL wire information_schema smoke via psql"
  psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -c "SELECT table_schema, table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name LIMIT 10;"
  psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -c "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = 'public' AND table_name = '$TABLE' ORDER BY ordinal_position;"

  echo "==> dbt catalog-shape introspection smoke"
  curl -fsS "$BASE_URL/api/v1/dbt/catalog" | python3 -c 'import json,sys; d=json.load(sys.stdin); assert "metadata" in d and "nodes" in d, d; print({"dbt_catalog_nodes": len(d["nodes"])})'

  echo "==> PostgreSQL wire unsupported COPY returns a clear error"
  set +e
  copy_output=$(psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d "$PGDATABASE" -c "COPY $TABLE TO STDOUT" 2>&1)
  copy_rc=$?
  set -e
  copy_expected_error_pattern="SQL_COMPATIBILITY|/api/v1/ingest|unsupported|not[ -]?supported"
  if [ "$copy_rc" -eq 0 ] || ! grep -Eiq "$copy_expected_error_pattern" <<<"$copy_output"; then
    echo "expected COPY $TABLE TO STDOUT to fail with a clear unsupported-error path; rc=$copy_rc" >&2
    echo "$copy_output" >&2
    exit 1
  fi
  echo "$copy_output" | head -20

  echo "==> PostgreSQL wire Python client lane (psycopg/psycopg2 extended query protocol is documented unsupported)"
  python3 - "$PGHOST" "$PGPORT" "$PGUSER" "$PGDATABASE" "$TABLE" <<'PY'
import importlib
import sys

host, port, user, dbname, table = sys.argv[1:]
conninfo = f"host={host} port={port} user={user} dbname={dbname}"


def expect_extended_protocol_error(client_name, fn):
    try:
        fn()
    except Exception as exc:
        message = str(exc)
        if "extended query protocol" not in message and "SQL_COMPATIBILITY" not in message:
            raise AssertionError(f"{client_name} failed without the expected clear extended query protocol message: {message}") from exc
        print({"client": client_name, "extended_query_protocol": "unsupported_clear_error"})
        return
    raise AssertionError(f"{client_name} unexpectedly succeeded; update docs/smoke if extended protocol support is implemented")


def run_psycopg(psycopg):
    with psycopg.connect(conninfo) as conn:
        with conn.cursor() as cur:
            cur.execute(f"SELECT COUNT(*) AS rows FROM {table};")
            cur.fetchone()


def run_psycopg2(psycopg2):
    conn = psycopg2.connect(conninfo)
    try:
        with conn.cursor() as cur:
            cur.execute(f"SELECT COUNT(*) AS rows FROM {table};")
            cur.fetchone()
    finally:
        conn.close()


ran_any = False
for client_name, module_name in (("psycopg", "psycopg"), ("psycopg2", "psycopg2")):
    try:
        module = importlib.import_module(module_name)
    except ImportError:
        if client_name == "psycopg":
            print("python pg client psycopg skipped: install psycopg for independent library smoke; default extended query protocol remains unsupported")
        else:
            print("python pg client psycopg2 skipped: install psycopg2 for independent library smoke; default extended query protocol remains unsupported")
        continue

    ran_any = True
    if client_name == "psycopg":
        expect_extended_protocol_error(client_name, lambda module=module: run_psycopg(module))
    else:
        expect_extended_protocol_error(client_name, lambda module=module: run_psycopg2(module))

if not ran_any:
    print("python pg clients skipped: install psycopg and/or psycopg2 for library smoke; default extended query protocol remains unsupported")
PY
fi

echo "OpenSnow public smoke checks passed."
