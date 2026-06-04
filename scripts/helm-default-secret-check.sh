#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-/tmp/opensnow-helm-default.yaml}"
HELM_IMAGE="${HELM_IMAGE:-alpine/helm:3.14.0}"
FORBIDDEN_DECODED="OPEN/SNOW/DEMO/ONLY/METADATA"
FORBIDDEN_B64="T1BFTi9TTk9XL0RFTU8vT05MWS9NRVRBREFUQQ=="

if command -v helm >/dev/null 2>&1; then
  (cd "$ROOT" && helm template opensnow deploy/helm/opensnow > "$OUT")
elif command -v docker >/dev/null 2>&1; then
  docker run --rm -v "$ROOT:/work" -w /work "$HELM_IMAGE" \
    template opensnow deploy/helm/opensnow > "$OUT"
else
  echo "helm-default-secret-check: need helm or docker to render chart" >&2
  exit 2
fi

if grep -q "$FORBIDDEN_B64" "$OUT"; then
  echo "helm-default-secret-check: forbidden legacy base64 metadata password rendered" >&2
  exit 1
fi

python3 - "$OUT" "$FORBIDDEN_DECODED" <<'PY'
import base64
import re
import sys

path, forbidden = sys.argv[1:]
text = open(path, encoding="utf-8").read()
for match in re.finditer(r'kind: Secret(?:(?!---).)*?name: opensnow-metadata(?:(?!---).)*?password: "?([^"\n]+)"?', text, re.S):
    value = match.group(1).strip()
    decoded = base64.b64decode(value).decode()
    if decoded == forbidden:
        raise SystemExit("helm-default-secret-check: forbidden legacy metadata password rendered")
print("helm-default-secret-check: default metadata password is not the legacy sentinel")
PY
