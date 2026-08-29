#!/usr/bin/env bash
# Run locally what CI runs on the runner, under the environment CI runs it in.
#
# During PR #23, eight consecutive green local passes missed a failure that only
# appeared on GHA: the local machine had a live Ollama the runner did not, so
# tests took the live path locally and the skip path in CI. The premise of a
# local mirror — green here means green there — was quietly false.
#
# Two properties keep this a mirror rather than a second list that rots:
#
#   1. The suite list is read out of .github/workflows/ci.yml, never copied.
#   2. The workflow's env block is asserted against what this script sets, so
#      adding a variable to CI without adding it here fails loudly instead of
#      opening a parity gap.
#
# Usage:
#   scripts/ci-mirror.sh              # everything except the browser job
#   scripts/ci-mirror.sh --with-e2e   # add the Playwright job
set -uo pipefail

cd "$(dirname "$0")/.."

WORKFLOW=".github/workflows/ci.yml"
WITH_E2E=0
[ "${1:-}" = "--with-e2e" ] && WITH_E2E=1

# ── Environment parity ────────────────────────────────────────────────────────
#
# These are ci.yml's top-level `env:` block, verbatim. The assertion below is
# what makes that claim checkable.
export CARGO_TERM_COLOR=always
export RUST_BACKTRACE=1
export SQLX_OFFLINE=true
export IONE_SKIP_LIVE=1
export DATABASE_URL="${DATABASE_URL:-postgres://ione:ione@localhost:5433/ione}"

expected="CARGO_TERM_COLOR=always IONE_SKIP_LIVE=1 RUST_BACKTRACE=1 SQLX_OFFLINE=true"
actual="$(python3 - "$WORKFLOW" <<'PY'
import re, sys
lines = open(sys.argv[1]).read().splitlines()
try:
    start = lines.index("env:") + 1
except ValueError:
    sys.exit("ci.yml has no top-level env: block")
out = []
for line in lines[start:]:
    if not line.startswith("  ") or not line.strip():
        break
    key, _, value = line.strip().partition(":")
    out.append(f"{key}={value.strip().strip(chr(34))}")
print(" ".join(sorted(out)))
PY
)"

if [ "$actual" != "$expected" ]; then
  cat >&2 <<MSG
Environment parity broken.

  ci.yml sets: $actual
  this mirror: $expected

A variable in CI that this script does not set is exactly how eight green local
runs missed a GHA failure. Update the export block above to match, then re-run.
MSG
  exit 2
fi

# CI has no Ollama. If one is reachable here, say so — IONE_SKIP_LIVE=1 keeps
# this run on the same path the runner takes, but a bare `cargo test` in this
# shell would not.
ollama="${OLLAMA_BASE_URL:-http://localhost:11434}"
if curl -fsS --max-time 2 "$ollama" >/dev/null 2>&1; then
  echo "note: a live Ollama is reachable at $ollama; CI has none."
  echo "      IONE_SKIP_LIVE=1 is forced here so this run takes the runner's path."
fi

failed=()
step() {
  local name="$1"; shift
  echo "── $name"
  if ! "$@"; then
    failed+=("$name")
    echo "   FAILED: $name"
  fi
}

# ── fmt · clippy · check ──────────────────────────────────────────────────────
step "cargo fmt --check"    cargo fmt --all -- --check
step "cargo clippy"         cargo clippy --all-targets -- -D warnings
step "cargo check"          cargo check --all-targets
step "contract citations"   python3 scripts/check-contract-citations.py
step "migrations append-only" bash scripts/check-migrations-immutable.sh

# ── integration tests ─────────────────────────────────────────────────────────
step "unit tests"           cargo test --lib --bins
step "phase01_chat"         cargo test --test phase01_chat

suites="$(python3 - "$WORKFLOW" <<'PY'
import re, sys
body = open(sys.argv[1]).read()
match = re.search(r"for p in (.*?); do", body, re.S)
if not match:
    sys.exit("could not find the suite list in ci.yml")
print(" ".join(match.group(1).replace("\\\n", " ").split()))
PY
)" || { echo "$suites" >&2; exit 2; }

echo "── integration suites ($(echo "$suites" | wc -w | tr -d ' ') read from $WORKFLOW)"
for p in $suites; do
  if ! out="$(cargo test --test "$p" -- --include-ignored --test-threads=1 2>&1)"; then
    failed+=("$p")
    echo "=== $p FAILED"
    echo "$out" | grep -E '^---- |panicked at' | head -4
  else
    echo "=== $p $(echo "$out" | grep -E '^test result' | tail -1)"
  fi
done

# ── playwright e2e ────────────────────────────────────────────────────────────
if [ "$WITH_E2E" = 1 ]; then
  step "build server" cargo build --bin ione
  step "playwright" npm run test:e2e
else
  echo "── playwright e2e SKIPPED (pass --with-e2e to run it)"
fi

echo
if [ ${#failed[@]} -eq 0 ]; then
  echo "CI mirror: everything green under the runner's environment."
  [ "$WITH_E2E" = 1 ] || echo "The browser job did not run here; CI will run it."
  exit 0
fi
echo "CI mirror: ${#failed[@]} failure(s): ${failed[*]}"
exit 1
