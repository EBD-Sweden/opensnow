#!/usr/bin/env bash
# Release a self-hosted OpenSnow demo snapshot to Cloud Run.
#
# This script is intentionally generic for the public OSS tree. It expects the
# operator to provide their own GCP project, build host, and optional dashboard
# URL via environment variables; do not commit operator-only project IDs, VM
# names, dashboard UUIDs, or secrets here.
#
# Usage:  ./release.sh [tag]        (default tag: demo-YYYYMMDD)
# Needs:  gcloud ADC locally; SSH access to a Linux build host with the demo
#         compose stack running and `demo-opensnow:latest` already built.
set -euo pipefail

BUILD_HOST="${OPENSNOW_CLOUDRUN_BUILD_HOST:?set OPENSNOW_CLOUDRUN_BUILD_HOST, e.g. ubuntu@build-host.example.com}"
PROJECT="${GCP_PROJECT:?set GCP_PROJECT to your Cloud Run project id}"
REGION="${GCP_REGION:-us-east1}"
SERVICE="${OPENSNOW_CLOUDRUN_SERVICE:-opensnow}"
TAG=${1:-demo-$(date +%Y%m%d)}
IMG="$REGION-docker.pkg.dev/$PROJECT/images/opensnow:$TAG"
HERE=$(cd "$(dirname "$0")" && pwd)

export PATH="$HOME/google-cloud-sdk/bin:$PATH"
TOKEN=$(gcloud auth application-default print-access-token)

echo "==> building $IMG on the build host (fresh /data snapshot)"
SRC_FILES=("$HERE/Dockerfile" "$HERE/entrypoint.sh" "$HERE/opensnow.toml" "$HERE/../demo/dashboards.json")
scp -q "${SRC_FILES[@]}" "$BUILD_HOST:/tmp/opensnow-cloudrun-src/" 2>/dev/null || {
  ssh -o BatchMode=yes "$BUILD_HOST" 'mkdir -p /tmp/opensnow-cloudrun-src'
  scp -q "${SRC_FILES[@]}" "$BUILD_HOST:/tmp/opensnow-cloudrun-src/"
}
ssh -o BatchMode=yes "$BUILD_HOST" bash -s "$IMG" "$TOKEN" <<'EOF'
set -euo pipefail
IMG=$1; TOKEN=$2
CTX=/tmp/opensnow-cloudrun; rm -rf "$CTX"; mkdir -p "$CTX"
cp /tmp/opensnow-cloudrun-src/* "$CTX/"
docker cp demo-opensnow-1:/data "$CTX/data"
cp -r /opt/opensnow/crates/opensnow-server/static "$CTX/ui"
docker build -t "$IMG" "$CTX"
echo "$TOKEN" | docker login -u oauth2accesstoken --password-stdin https://us-east1-docker.pkg.dev
docker push "$IMG"
EOF

echo "==> deploying to Cloud Run"
export CLOUDSDK_AUTH_ACCESS_TOKEN=$TOKEN
ENV_VARS="OPENSNOW_ALLOW_PUBLIC=1,RUST_LOG=warn,OPENSNOW_DBT_ARTIFACTS_DIR=/data/dbt-target,OPENSNOW_QUERY_TIMEOUT_SECS=20,OPENSNOW_CHARTS_FILE=/data/charts.json"
if [[ -n "${OPENSNOW_DASHBOARD_URL:-}" ]]; then
  ENV_VARS="$ENV_VARS,OPENSNOW_DASHBOARD_URL=$OPENSNOW_DASHBOARD_URL,OPENSNOW_DASHBOARD_NAME=${OPENSNOW_DASHBOARD_NAME:-External dashboard}"
fi

gcloud run deploy "$SERVICE" --image="$IMG" --region="$REGION" --project="$PROJECT" \
  --allow-unauthenticated --min-instances=0 --max-instances=1 --concurrency=40 \
  --memory=1Gi --cpu=1 --port=8080 --timeout=60 \
  --set-env-vars="$ENV_VARS"

URL=$(gcloud run services describe "$SERVICE" --region="$REGION" --project="$PROJECT" --format='value(status.url)')
echo "==> smoke test: $URL"
curl -sS -m 30 -o /dev/null -w "SPA HTTP %{http_code} (%{time_total}s)\n" "$URL/"
curl -sS -m 30 -X POST "$URL/api/v1/query" -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT count(*) AS marts FROM mart_portfolio_outcome"}' ; echo
