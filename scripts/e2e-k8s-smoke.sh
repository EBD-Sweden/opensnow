#!/usr/bin/env bash
# OpenSnow local Kubernetes smoke runner.
#
# Safe by default: without RUN_E2E=1 it only prints the execution plan.
# Requires: docker, kind, kubectl, helm (or HELM='docker run ... helm' wrapper).
# Optional env:
#   RUN_E2E=1        actually create/use kind and deploy
#   CLEANUP=1        uninstall release and delete kind cluster at the end
#   CLUSTER_NAME     default: opensnow-e2e
#   RELEASE          default: opensnow
#   NAMESPACE        default: opensnow-e2e
#   IMAGE_TAG        default: p2a-helm-test
#   CHART            default: deploy/helm/opensnow

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER_NAME="${CLUSTER_NAME:-opensnow-e2e}"
RELEASE="${RELEASE:-opensnow}"
NAMESPACE="${NAMESPACE:-opensnow-e2e}"
IMAGE_REPO="${IMAGE_REPO:-opensnow}"
IMAGE_TAG="${IMAGE_TAG:-p2a-helm-test}"
CHART="${CHART:-deploy/helm/opensnow}"
HELM="${HELM:-helm}"
HELM_IMAGE="${HELM_IMAGE:-alpine/helm:3.16.4}"
HELM_HOST_DIR="${HELM_HOST_DIR:-/tmp/opensnow-helm}"
KIND="${KIND:-kind}"
KUBECTL="${KUBECTL:-kubectl}"
RUN_E2E="${RUN_E2E:-0}"
CLEANUP="${CLEANUP:-0}"

cd "$ROOT"

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 127
  }
}

helm_cmd() {
  if command -v "$HELM" >/dev/null 2>&1; then
    "$HELM" "$@"
  else
    mkdir -p "$HELM_HOST_DIR/cache" "$HELM_HOST_DIR/config" "$HELM_HOST_DIR/data"
    docker run --rm --entrypoint helm \
      --network host \
      -e KUBECONFIG=/root/.kube/config \
      -e HELM_CACHE_HOME=/tmp/helm-cache \
      -e HELM_CONFIG_HOME=/tmp/helm-config \
      -e HELM_DATA_HOME=/tmp/helm-data \
      -v "$HELM_HOST_DIR/cache:/tmp/helm-cache" \
      -v "$HELM_HOST_DIR/config:/tmp/helm-config" \
      -v "$HELM_HOST_DIR/data:/tmp/helm-data" \
      -v "$HOME/.kube:/root/.kube:ro" \
      -v "$ROOT:/repo" \
      -w /repo \
      "$HELM_IMAGE" "$@"
  fi
}

helm_init_repos() {
  helm_cmd repo add bitnami https://charts.bitnami.com/bitnami >/dev/null 2>&1 || true
  helm_cmd repo add minio https://charts.min.io/ >/dev/null 2>&1 || true
  helm_cmd repo update
}

fix_chart_ownership() {
  docker run --rm \
    -v "$ROOT:/repo" \
    alpine:latest \
    sh -c "chown -R $(id -u):$(id -g) /repo/$CHART/Chart.lock /repo/$CHART/charts 2>/dev/null || true"
}

print_plan() {
  cat <<PLAN
OpenSnow E2E smoke plan (dry-run)

Cluster:   kind/$CLUSTER_NAME
Namespace: $NAMESPACE
Release:   $RELEASE
Image:     $IMAGE_REPO:$IMAGE_TAG
Chart:     $CHART

This script will, when RUN_E2E=1:
  1. verify docker/kind/kubectl/helm are available
  2. create kind cluster if missing
  3. load local image $IMAGE_REPO:$IMAGE_TAG into kind
  4. helm dependency build $CHART
  5. helm upgrade --install $RELEASE $CHART with dev values and local image override
  6. wait for coordinator, worker, metadata, MCP pods
  7. verify Redis responds to PING
  8. port-forward gateway and run HTTP health + query smokes
  9. print pod/service status and recent logs on failure

No cluster is deleted unless CLEANUP=1.

Run:
  RUN_E2E=1 bash scripts/e2e-k8s-smoke.sh

Cleanup run:
  RUN_E2E=1 CLEANUP=1 bash scripts/e2e-k8s-smoke.sh
PLAN
}

if [[ "$RUN_E2E" != "1" ]]; then
  print_plan
  exit 0
fi

require docker
require "$KIND"
require "$KUBECTL"

if ! docker image inspect "$IMAGE_REPO:$IMAGE_TAG" >/dev/null 2>&1; then
  echo "missing local image $IMAGE_REPO:$IMAGE_TAG; build it first:" >&2
  echo "  docker build -t $IMAGE_REPO:$IMAGE_TAG ." >&2
  exit 1
fi

cleanup() {
  if [[ "$CLEANUP" == "1" ]]; then
    echo "cleanup: uninstalling $RELEASE and deleting kind cluster $CLUSTER_NAME"
    helm_cmd uninstall "$RELEASE" -n "$NAMESPACE" >/dev/null 2>&1 || true
    "$KIND" delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

if ! "$KIND" get clusters | grep -qx "$CLUSTER_NAME"; then
  echo "creating kind cluster $CLUSTER_NAME"
  "$KIND" create cluster --name "$CLUSTER_NAME"
fi

"$KUBECTL" config use-context "kind-$CLUSTER_NAME" >/dev/null
"$KUBECTL" create namespace "$NAMESPACE" --dry-run=client -o yaml | "$KUBECTL" apply -f -

echo "loading image $IMAGE_REPO:$IMAGE_TAG into kind/$CLUSTER_NAME"
"$KIND" load docker-image "$IMAGE_REPO:$IMAGE_TAG" --name "$CLUSTER_NAME"

echo "building helm dependencies"
helm_init_repos
helm_cmd dependency build "$CHART"
fix_chart_ownership

echo "deploying $RELEASE"
helm_cmd upgrade --install "$RELEASE" "$CHART" \
  -n "$NAMESPACE" \
  -f "$CHART/values-dev.yaml" \
  --set image.repository="$IMAGE_REPO" \
  --set image.tag="$IMAGE_TAG" \
  --set image.pullPolicy=Never \
  --set gateway.type=NodePort \
  --wait \
  --timeout 10m

echo "waiting for core workloads"
"$KUBECTL" wait -n "$NAMESPACE" --for=condition=Ready pod \
  -l app.kubernetes.io/instance="$RELEASE" \
  --timeout=10m

"$KUBECTL" get pods,svc -n "$NAMESPACE" -o wide

redis_pod=$("$KUBECTL" get pod -n "$NAMESPACE" \
  -l app.kubernetes.io/instance="$RELEASE",app.kubernetes.io/name=redis,app.kubernetes.io/component=master \
  -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || true)
if [[ -n "$redis_pod" ]]; then
  echo "checking Redis PING via pod/$redis_pod"
  "$KUBECTL" exec -n "$NAMESPACE" "$redis_pod" -- sh -c \
    'redis-cli ping 2>/dev/null || /opt/bitnami/redis/bin/redis-cli ping' | grep -qx PONG
  echo "redis smoke: PASS PING"
else
  echo "redis smoke: WARN — Redis pod not found" >&2
fi

svc="${RELEASE}-gateway"
echo "port-forwarding svc/$svc for HTTP/query smoke"
"$KUBECTL" port-forward -n "$NAMESPACE" "svc/$svc" 18080:8080 >/tmp/opensnow-e2e-port-forward.log 2>&1 &
pf_pid=$!
sleep 3
trap 'kill "$pf_pid" >/dev/null 2>&1 || true; cleanup' EXIT

if command -v curl >/dev/null 2>&1; then
  if curl -fsS http://127.0.0.1:18080/health >/tmp/opensnow-e2e-health.out 2>/tmp/opensnow-e2e-health.err; then
    echo "health smoke: PASS /health"
  else
    echo "health smoke: FAIL — /health did not succeed; preserving cluster for inspection" >&2
    echo "--- port-forward log ---" >&2
    cat /tmp/opensnow-e2e-port-forward.log >&2 || true
    echo "--- recent coordinator logs ---" >&2
    "$KUBECTL" logs -n "$NAMESPACE" -l app.kubernetes.io/component=coordinator --tail=100 >&2 || true
    exit 2
  fi

  if curl -fsS \
    -H 'content-type: application/json' \
    -d '{"sql":"SELECT 1 AS smoke"}' \
    http://127.0.0.1:18080/api/v1/query >/tmp/opensnow-e2e-query.out 2>/tmp/opensnow-e2e-query.err \
    && grep -q '"status":"ok"' /tmp/opensnow-e2e-query.out \
    && grep -q '"rows":1' /tmp/opensnow-e2e-query.out \
    && grep -q '\\"smoke\\":1' /tmp/opensnow-e2e-query.out; then
    echo "query smoke: PASS SELECT 1 AS smoke"
  else
    echo "query smoke: FAIL — /api/v1/query did not return expected SELECT result" >&2
    echo "--- query response ---" >&2
    cat /tmp/opensnow-e2e-query.out >&2 || true
    echo "--- query error ---" >&2
    cat /tmp/opensnow-e2e-query.err >&2 || true
    echo "--- recent coordinator logs ---" >&2
    "$KUBECTL" logs -n "$NAMESPACE" -l app.kubernetes.io/component=coordinator --tail=100 >&2 || true
    exit 3
  fi
else
  echo "curl unavailable; skipped HTTP smoke"
fi

echo "OpenSnow E2E smoke completed"
