#!/usr/bin/env bash
# =============================================================================
# dep-check.sh — OpenSnow dependency health check
#
# Run this at the start of every development session to catch:
#   1. Duplicate crates (same crate, multiple versions compiled)
#   2. Unused dependencies (dead weight in Cargo.toml)
#   3. Security vulnerabilities (cargo-audit)
#   4. Outdated dependencies (cargo-outdated)
#
# Usage:
#   bash scripts/dep-check.sh           # full check
#   bash scripts/dep-check.sh --fast    # duplicates only (no network)
#   bash scripts/dep-check.sh --fix     # auto-fix what's fixable
# =============================================================================

set -euo pipefail
CARGO="${CARGO:-$HOME/.cargo/bin/cargo}"
BOLD="\033[1m"; CYAN="\033[0;36m"; GREEN="\033[0;32m"
RED="\033[0;31m"; YELLOW="\033[1;33m"; NC="\033[0m"

FAST=false; FIX=false
for arg in "$@"; do
  [[ "$arg" == "--fast" ]] && FAST=true
  [[ "$arg" == "--fix"  ]] && FIX=true
done

cd "$(dirname "$0")/.."

echo
echo -e "${BOLD}============================================================${NC}"
echo -e "${BOLD}  OpenSnow Dependency Health Check${NC}"
echo -e "${BOLD}============================================================${NC}"
echo

# ── 1. Duplicate crates ───────────────────────────────────────────────────────
echo -e "${BOLD}[1] Duplicate crates${NC}"
echo -e "    ${CYAN}cargo tree --duplicates${NC}"
echo

DUPES=$($CARGO tree --duplicates 2>/dev/null | grep -v "Downloading\|Downloaded\|Compiling\|Locking" | grep -E "^[a-z]" | grep -v "^\s" | sort -u || true)

if [[ -z "$DUPES" ]]; then
  echo -e "    ${GREEN}✓ No duplicate crates${NC}"
else
  # Known/acceptable duplicates — these are caused by transitive deps we don't control
  # and cannot be resolved without a major Arrow/DataFusion ecosystem upgrade.
  KNOWN_DUPES=(
    "axum"            # axum 0.7 (via tonic+arrow-flight) vs 0.8 (our code) — resolve when arrow-flight >= 58
    "axum-core"       # internal axum core crate, versioned with axum
    "tower"           # tower 0.4 (tonic 0.12) vs 0.5 — same root cause
    "bitflags"        # v1 (flatbuffers/legacy) vs v2 — industry-wide migration
    "hashbrown"       # multiple versions across different crates — unavoidable
    "rand"            # v0.8 (many crates) vs v0.9 — ecosystem in migration
    "rand_core"       # same as rand
    "rand_chacha"     # same as rand
    "getrandom"       # v0.2/v0.3/v0.4 — follows rand migration
    "thiserror"       # v1 vs v2 — many crates still on v1
    "thiserror-impl"  # proc-macro for thiserror (v1 vs v2)
    "indexmap"        # v1 vs v2 — common
    "itertools"       # v0.13 vs v0.14 — common
    "fallible-iterator" # v0.2 vs v0.3
    "serde_json"      # multiple minor — all compatible
    "serde_core"      # internal serde crate
    "prost"           # follows tonic version
    "prost-types"     # follows tonic version
    "tonic"           # v0.12 (arrow-flight) vs workspace — resolve when arrow-flight >= 58
    "chrono"          # single version, false positive
    "matchit"         # axum routing internal
    "socket2"         # OS abstraction, minor versions
    "log"             # legacy logging facade
    "twox-hash"       # minor
    "untrusted"       # minor
    "tokio"           # all v1.x — compatible
    "petgraph"        # datafusion internal
    "datafusion-common" # datafusion internal
    "datafusion-expr"   # datafusion internal
  )

  ACTIONABLE=()
  while IFS= read -r line; do
    crate=$(echo "$line" | cut -d' ' -f1)
    is_known=false
    for known in "${KNOWN_DUPES[@]}"; do
      [[ "$crate" == "$known" ]] && is_known=true && break
    done
    if [[ "$is_known" == "false" ]]; then
      ACTIONABLE+=("$line")
    fi
  done <<< "$DUPES"

  KNOWN_COUNT=$(echo "$DUPES" | wc -l)
  echo -e "    ${YELLOW}⚠ $KNOWN_COUNT duplicate crates found${NC}"
  echo

  if [[ ${#ACTIONABLE[@]} -gt 0 ]]; then
    echo -e "    ${RED}ACTION REQUIRED — new duplicates (not in known list):${NC}"
    for line in "${ACTIONABLE[@]}"; do
      echo -e "      ${RED}→ $line${NC}"
    done
    echo
  else
    echo -e "    ${GREEN}✓ All duplicates are known/accepted (transitive, ecosystem-wide)${NC}"
  fi

  echo -e "    ${CYAN}Known/accepted duplicates:${NC}"
  while IFS= read -r line; do
    echo -e "      ${YELLOW}· $line${NC}"
  done <<< "$DUPES"
  echo

  echo -e "    ${BOLD}Resolution plan:${NC}"
  echo -e "      axum/tower/tonic: blocked on arrow-flight ≥ 58 (requires Arrow 58 + DataFusion bump)"
  echo -e "      bitflags/rand/thiserror: resolve naturally as ecosystem migrates"
  echo -e "      Everything else: transitive, no action needed"
fi

if [[ "$FAST" == "true" ]]; then
  echo -e "${YELLOW}--fast mode: skipping network checks${NC}"
  echo
  exit 0
fi

# ── 2. Unused dependencies (cargo-machete) ────────────────────────────────────
echo -e "${BOLD}[2] Unused dependencies${NC}"
echo -e "    ${CYAN}cargo machete${NC}"
echo

if ! command -v cargo-machete &>/dev/null && ! $CARGO machete --version &>/dev/null 2>&1; then
  echo -e "    ${YELLOW}cargo-machete not installed — installing...${NC}"
  $CARGO install cargo-machete --quiet
fi

MACHETE_OUT=$($CARGO machete 2>&1 || true)
if echo "$MACHETE_OUT" | grep -q "unused"; then
  echo -e "    ${RED}Unused dependencies found:${NC}"
  echo "$MACHETE_OUT" | grep -v "^$" | while read -r line; do
    echo -e "      $line"
  done
  if [[ "$FIX" == "true" ]]; then
    echo -e "    ${CYAN}Auto-fixing with --fix...${NC}"
    $CARGO machete --fix 2>&1 || true
    echo -e "    ${GREEN}Fixed. Re-run dep-check to verify.${NC}"
  else
    echo -e "    ${YELLOW}Run with --fix to auto-remove, or edit Cargo.toml manually${NC}"
  fi
else
  echo -e "    ${GREEN}✓ No unused dependencies${NC}"
fi
echo

# ── 3. Security audit (cargo-audit) ───────────────────────────────────────────
echo -e "${BOLD}[3] Security audit${NC}"
echo -e "    ${CYAN}cargo audit${NC}"
echo

if ! command -v cargo-audit &>/dev/null && ! $CARGO audit --version &>/dev/null 2>&1; then
  echo -e "    ${YELLOW}cargo-audit not installed — installing...${NC}"
  $CARGO install cargo-audit --quiet
fi

AUDIT_OUT=$($CARGO audit 2>&1 || true)
if echo "$AUDIT_OUT" | grep -qiE "error\[|vulnerability|RUSTSEC"; then
  echo -e "    ${RED}Security issues found:${NC}"
  echo "$AUDIT_OUT" | grep -iE "error\[|RUSTSEC|vulnerability|warning\[" | while read -r line; do
    echo -e "      ${RED}→ $line${NC}"
  done
  echo -e "    ${YELLOW}See https://rustsec.org for details${NC}"
else
  echo -e "    ${GREEN}✓ No known vulnerabilities${NC}"
fi
echo

# ── 4. Outdated direct dependencies ───────────────────────────────────────────
echo -e "${BOLD}[4] Outdated direct workspace dependencies${NC}"
echo -e "    ${CYAN}cargo outdated --root-deps-only${NC}"
echo

if ! $CARGO outdated --version &>/dev/null 2>&1; then
  echo -e "    ${YELLOW}cargo-outdated not installed — installing...${NC}"
  $CARGO install cargo-outdated --quiet
fi

OUTDATED_OUT=$($CARGO outdated --root-deps-only --workspace 2>&1 || true)
if echo "$OUTDATED_OUT" | grep -q "---"; then
  echo "$OUTDATED_OUT" | grep -v "^$\|Checking\|Fetching" | while read -r line; do
    echo -e "      $line"
  done
else
  echo -e "    ${GREEN}✓ All direct dependencies are up to date${NC}"
fi
echo

# ── 5. Summary ────────────────────────────────────────────────────────────────
echo -e "${BOLD}============================================================${NC}"
echo -e "${BOLD}  Summary${NC}"
echo -e "${BOLD}============================================================${NC}"
echo
echo -e "  Run ${CYAN}bash scripts/dep-check.sh --fast${NC}   → duplicates only (5s)"
echo -e "  Run ${CYAN}bash scripts/dep-check.sh${NC}          → full check (~30s)"
echo -e "  Run ${CYAN}bash scripts/dep-check.sh --fix${NC}    → auto-fix unused deps"
echo
echo -e "  ${BOLD}Next planned ecosystem upgrade:${NC}"
echo -e "  When arrow-flight ≥ 58 is compatible with datafusion, bump:"
echo -e "    arrow/parquet/datafusion → 58.x"
echo -e "    arrow-flight → 58.x"
echo -e "    tonic → 0.14"
echo -e "  This resolves: axum 0.7 vs 0.8, tower 0.4 vs 0.5"
echo
