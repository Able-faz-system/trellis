#!/usr/bin/env bash
# Minimal TTL "keeper": calls `trellis extend-ttl` for every tracked agreement
# so idle agreements are not archived when their ledger TTL runs out.
#
# This is a stopgap, not a production keeper network — see DEPLOYMENT.md
# ("Agreement TTL Keeper") for its assumptions and limitations.
#
# Usage:
#   scripts/ttl-keeper.sh [AGREEMENTS_FILE]
#
# AGREEMENTS_FILE defaults to .github/keeper-agreements.txt: one 64-char hex
# agreement ID per line; blank lines and `#` comments are ignored. IDs may
# also be supplied (comma- or whitespace-separated) via
# TRELLIS_KEEPER_AGREEMENT_IDS, which replaces the file when set.
#
# Required environment:
#   TRELLIS_CONTRACT_ID     contract the agreements live in
#   TRELLIS_SOURCE_KEY      fee-paying identity name or S… secret key
#   TRELLIS_KEEPER_CALLER   public G… address recorded as `caller` in the
#                           ttl_extended event (normally the source key's)
# Optional:
#   TRELLIS_BIN             path to the trellis binary (default: trellis)
#
# Every agreement is attempted even if an earlier one fails; the script exits
# non-zero at the end if any extension failed.
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

AGREEMENTS_FILE="${1:-.github/keeper-agreements.txt}"
TRELLIS_BIN="${TRELLIS_BIN:-trellis}"

for var in TRELLIS_CONTRACT_ID TRELLIS_SOURCE_KEY TRELLIS_KEEPER_CALLER; do
  if [[ -z "${!var:-}" ]]; then
    echo "error: $var must be set" >&2
    exit 2
  fi
done

if [[ -n "${TRELLIS_KEEPER_AGREEMENT_IDS:-}" ]]; then
  ids=$(tr ',' '\n' <<<"$TRELLIS_KEEPER_AGREEMENT_IDS")
elif [[ -f "$AGREEMENTS_FILE" ]]; then
  ids=$(sed -e 's/#.*//' "$AGREEMENTS_FILE")
else
  echo "error: agreements file $AGREEMENTS_FILE not found" >&2
  exit 2
fi

ok=0
failed=()
for id in $ids; do
  echo "==> Extending TTL for agreement $id"
  if "$TRELLIS_BIN" extend-ttl \
      --agreement-id "$id" \
      --caller "$TRELLIS_KEEPER_CALLER" \
      --yes; then
    ok=$((ok + 1))
  else
    failed+=("$id")
  fi
done

echo "==> Extended $ok agreement(s); ${#failed[@]} failed"
if (( ${#failed[@]} > 0 )); then
  printf '    failed: %s\n' "${failed[@]}" >&2
  exit 1
fi
