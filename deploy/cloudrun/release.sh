#!/usr/bin/env bash
# Release the hosted OpenSnow demo to Cloud Run (project opensnow-prod, us-east1).
#
# Pattern: docs/gcp-cloudrun-hosting-playbook.md (EBD-Sweden/docs). The image is
# derived ON the Hetzner VM from the compose-built `demo-opensnow:latest`
# (already linux/amd64), baking in: the SPA (/ui), a Cloud-Run config (no
# pgwire, single $PORT), and a live snapshot of the demo warehouse (/data,
# ~20MB) so scale-to-zero cold starts serve the full demo statelessly.
#
# Usage:  ./release.sh [tag]        (default tag: demo-YYYYMMDD)
# Needs:  gcloud ADC locally; ssh root@VM; the VM compose stack running.
set -euo pipefail

VM=root@REDACTED-VM
PROJECT=opensnow-prod
REGION=us-east1
TAG=${1:-demo-$(date +%Y%m%d)}
IMG=$REGION-docker.pkg.dev/$PROJECT/images/opensnow:$TAG
HERE=$(cd "$(dirname "$0")" && pwd)

export PATH="$HOME/google-cloud-sdk/bin:$PATH"
TOKEN=$(gcloud auth application-default print-access-token)

echo "==> building $IMG on the VM (fresh /data snapshot)"
scp -q "$HERE/Dockerfile" "$HERE/entrypoint.sh" "$HERE/opensnow.toml" "$VM:/tmp/opensnow-cloudrun-src/" 2>/dev/null || {
  ssh -o BatchMode=yes "$VM" 'mkdir -p /tmp/opensnow-cloudrun-src'
  scp -q "$HERE/Dockerfile" "$HERE/entrypoint.sh" "$HERE/opensnow.toml" "$VM:/tmp/opensnow-cloudrun-src/"
}
ssh -o BatchMode=yes "$VM" bash -s "$IMG" "$TOKEN" <<'EOF'
set -euo pipefail
IMG=$1; TOKEN=$2
CTX=/tmp/opensnow-cloudrun; rm -rf $CTX; mkdir -p $CTX
cp /tmp/opensnow-cloudrun-src/* $CTX/
docker cp demo-opensnow-1:/data $CTX/data
cp -r /opt/opensnow/crates/opensnow-server/static $CTX/ui
docker build -t "$IMG" $CTX
echo "$TOKEN" | docker login -u oauth2accesstoken --password-stdin https://us-east1-docker.pkg.dev
docker push "$IMG"
EOF

echo "==> deploying to Cloud Run"
export CLOUDSDK_AUTH_ACCESS_TOKEN=$TOKEN
gcloud run deploy opensnow --image="$IMG" --region=$REGION --project=$PROJECT \
  --allow-unauthenticated --min-instances=0 --max-instances=2 \
  --memory=1Gi --cpu=1 --port=8080 --timeout=60 \
  --set-env-vars="OPENSNOW_ALLOW_PUBLIC=1,RUST_LOG=warn,OPENSNOW_DBT_ARTIFACTS_DIR=/data/dbt-target,OPENSNOW_QUERY_TIMEOUT_SECS=20,OPENSNOW_CHARTS_FILE=/data/charts.json,OPENSNOW_DASHBOARD_URL=https://metabase.ebdsweden.com/public/dashboard/00769301-ca5e-49b9-8626-8ce33dd01ea9,OPENSNOW_DASHBOARD_NAME=The Krona's Bargain"

URL=$(gcloud run services describe opensnow --region=$REGION --project=$PROJECT --format='value(status.url)')
echo "==> smoke test: $URL"
curl -sS -m 30 -o /dev/null -w "SPA HTTP %{http_code} (%{time_total}s)\n" "$URL/"
curl -sS -m 30 -X POST "$URL/api/v1/query" -H 'Content-Type: application/json' \
  -d '{"sql":"SELECT count(*) AS marts FROM mart_portfolio_outcome"}' ; echo
