#!/usr/bin/env bash
# One-time seed: build the dbt marts inside OpenSnow, then export them to the
# Postgres serving layer that Metabase reads. Idempotent — safe to re-run.
set -euo pipefail

OPENSNOW_HTTP="${OPENSNOW_HTTP:-http://opensnow:8080}"
TARGET_PG_DSN="${TARGET_PG_DSN:?set TARGET_PG_DSN}"

echo "==> waiting for OpenSnow ($OPENSNOW_HTTP) ..."
for i in $(seq 1 60); do
  if curl -fsS "$OPENSNOW_HTTP/health" >/dev/null 2>&1; then break; fi
  sleep 2
done
curl -fsS "$OPENSNOW_HTTP/health" >/dev/null

echo "==> source tables registered in OpenSnow:"
curl -fsS -X POST "$OPENSNOW_HTTP/api/v1/query" -H 'content-type: application/json' \
  -d '{"sql":"SHOW TABLES"}' | head -c 600 || true
echo

echo "==> dbt run (build staging + marts in OpenSnow, in dependency order)"
cd /work/dbt
DBT_PROFILES_DIR=/work/dbt dbt run --no-partial-parse

echo "==> export marts to Postgres for Metabase"
for mart in mart_house_price_index mart_house_price_yoy mart_house_price_latest mart_gdp_growth_qoq; do
  echo "   -> eurostat.$mart"
  curl -fsS -X POST "$OPENSNOW_HTTP/api/v1/export/postgres" -H 'content-type: application/json' \
    -d "{\"sql\":\"SELECT * FROM $mart\",\"dsn\":\"$TARGET_PG_DSN\",\"schema\":\"eurostat\",\"table\":\"$mart\",\"mode\":\"replace\"}"
  echo
done

echo "==> seed complete. Marts are in OpenSnow and in Postgres (schema eurostat)."
