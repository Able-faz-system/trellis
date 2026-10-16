#!/usr/bin/env bash
# Probes the documented live testnet deployment with `trellis status` and
# classifies the result, so a testnet reset is detected automatically instead
# of by a contributor noticing "agreement not found" (#491).
#
# Usage:
#   scripts/check-testnet-health.sh [AGREEMENT_ID]
#
# AGREEMENT_ID defaults to the Verified Test Agreement in DEPLOYMENT.md.
#
# Required environment (same as the CLI):
#   TRELLIS_CONTRACT_ID, TRELLIS_SOURCE_KEY
# Optional:
#   TRELLIS_BIN   path to the trellis binary (default: trellis)
#
# Exit codes:
#   0   healthy — the agreement resolved
#   10  reset suspected — the contract or agreement no longer exists on-chain
#   1   inconclusive — the probe failed for another reason (RPC outage,
#       misconfiguration, …); NOT treated as a reset, so a flaky RPC never
#       raises a false alarm
#
# The probe output is also written to $PROBE_LOG (default: probe.log).
set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

AGREEMENT_ID="${1:-0101010101010101010101010101010101010101010101010101010101010101}"
TRELLIS_BIN="${TRELLIS_BIN:-trellis}"
PROBE_LOG="${PROBE_LOG:-probe.log}"

echo "==> trellis status --agreement-id $AGREEMENT_ID (contract $TRELLIS_CONTRACT_ID)"
"$TRELLIS_BIN" status --agreement-id "$AGREEMENT_ID" >"$PROBE_LOG" 2>&1
status=$?
cat "$PROBE_LOG"

if [[ $status -eq 0 ]]; then
  echo "==> Healthy: agreement resolved"
  exit 0
fi

# Signatures of state that is gone rather than temporarily unreachable:
#   Error(Contract, #3)        TrellisError::AgreementNotFound
#   MissingValue / not found   the contract instance itself no longer exists
if grep -qiE 'Error\(Contract, #3\)|AgreementNotFound|agreement not found|MissingValue|contract (instance |code )?not found|contract .* does not exist' "$PROBE_LOG"; then
  echo "==> Reset suspected: contract or agreement no longer exists on testnet"
  exit 10
fi

echo "==> Inconclusive: probe failed for a reason other than missing state"
exit 1
