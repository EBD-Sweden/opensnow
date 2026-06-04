#!/usr/bin/env python3
"""Load the deterministic OpenSnow public demo manifest through the REST API."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


def post_json(base_url: str, path: str, payload: dict[str, Any]) -> dict[str, Any]:
    data = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        f"{base_url.rstrip('/')}{path}",
        data=data,
        headers={"content-type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"POST {path} failed with HTTP {exc.code}: {body}") from exc


def load_manifest(path: Path) -> dict[str, Any]:
    manifest = json.loads(path.read_text())
    if not manifest.get("synthetic") or not manifest.get("deterministic"):
        raise ValueError("demo manifest must be synthetic and deterministic")
    if manifest.get("contains_real_customer_data") is not False:
        raise ValueError("demo manifest must not contain real customer data")
    return manifest


def seed(base_url: str, manifest: dict[str, Any]) -> None:
    for table in manifest["tables"]:
        payload = {
            "table": table["name"],
            "columns": table["columns"],
            "rows": table["rows"],
            "replace": True,
        }
        result = post_json(base_url, "/api/v1/ingest", payload)
        if result.get("status") != "ok":
            raise RuntimeError(f"ingest failed for {table['name']}: {result}")
        expected = table["row_count"]
        actual = result.get("rows_ingested")
        if actual != expected:
            raise RuntimeError(f"{table['name']} ingested {actual}, expected {expected}")
        print(f"loaded {table['name']}: {expected} rows")


def verify(base_url: str, manifest: dict[str, Any]) -> None:
    for check in manifest.get("checks", []):
        result = post_json(base_url, "/api/v1/query", {"sql": check["sql"]})
        if result.get("status") != "ok":
            raise RuntimeError(f"query check failed for {check['name']}: {result}")
        rows = result.get("rows")
        if isinstance(rows, int):
            actual_rows = rows
        elif isinstance(rows, list):
            actual_rows = len(rows)
        else:
            actual_rows = 0
        expected_rows = check["expected_rows"]
        if actual_rows < expected_rows:
            raise RuntimeError(
                f"query check {check['name']} returned {actual_rows}, expected at least {expected_rows}"
            )
        print(f"verified {check['name']}: {actual_rows} row(s)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://localhost:8080")
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()

    manifest = load_manifest(args.manifest)
    print(f"seeding {manifest['name']} from {args.manifest}")
    seed(args.base_url, manifest)
    verify(args.base_url, manifest)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:  # noqa: BLE001 - CLI should print concise failures.
        print(f"demo seed failed: {exc}", file=sys.stderr)
        raise SystemExit(1)
