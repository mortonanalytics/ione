#!/usr/bin/env bash
# An applied migration is immutable.
#
# A one-line comment edit to an already-applied migration changed its sqlx
# checksum, which broke `sqlx migrate run` against every existing database and
# took down all 57 integration suites at once (fixed in 7756e89 by moving the
# note into a new migration). Nothing stopped it from happening again.
#
# This fails when a migration file that already exists on the base branch is
# modified, deleted, or renamed. Adding a new migration is untouched.
#
# Usage:
#   scripts/check-migrations-immutable.sh [base-ref]     # default: origin/main
set -euo pipefail

base_ref="${1:-origin/main}"

if ! git rev-parse --verify --quiet "$base_ref" >/dev/null; then
  git fetch --no-tags --depth=0 origin main >/dev/null 2>&1 || true
fi
if ! git rev-parse --verify --quiet "$base_ref" >/dev/null; then
  echo "cannot resolve $base_ref; fetch it before running this check" >&2
  exit 2
fi

base="$(git merge-base "$base_ref" HEAD)"

# --diff-filter excludes A (added), so a new migration never trips this.
changed="$(git diff --name-status --diff-filter=MDR "$base" HEAD -- migrations/ || true)"

if [ -z "$changed" ]; then
  echo "migrations are append-only: no existing migration was modified"
  exit 0
fi

echo "An already-committed migration was changed:" >&2
echo "$changed" >&2
cat >&2 <<'MSG'

sqlx records a checksum for every migration it applies. Editing a file it has
already run — even a comment — makes that checksum mismatch, and every existing
database refuses to migrate. That is not a local problem: it breaks CI, every
developer checkout, and every deployed node at once.

Add a new migration instead. If the change is only a comment or a note, it
belongs in the newest migration's header, not in the old file.
MSG
exit 1
