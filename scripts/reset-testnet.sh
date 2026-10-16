#!/usr/bin/env bash
# Redeploys the Trellis contract to Stellar testnet and creates a fresh test
# agreement. Use this after a testnet reset invalidates the contract ID or
# agreement ID referenced in DEPLOYMENT.md.
#
# Before touching any files it verifies its own output end-to-end: the new
# agreement is queried back from the chain, and milestone 0 is funded with a
# real lock_funds token transfer from the payer, so a reset that produced an
# unusable agreement (e.g. a non-token `--token` address) fails loudly here
# instead of surfacing later.
#
# On success it writes the new values back into DEPLOYMENT.md (the
# `reset-testnet:begin/end` marker blocks), frontend/.env and
# frontend/.env.example, so no manual copy-paste is needed. Nothing is
# written if any step before that fails.
#
# Environment overrides:
#   FRONTEND_ENV_FILE  env file to patch instead of frontend/.env
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

RPC_URL="https://soroban-testnet.stellar.org"
NETWORK_PASSPHRASE="Test SDF Network ; September 2015"
DEPLOYER_KEY="trellis-deployer"
DEPLOYMENT_DOC="DEPLOYMENT.md"
FRONTEND_ENV_FILE="${FRONTEND_ENV_FILE:-frontend/.env}"
FRONTEND_ENV_EXAMPLE="frontend/.env.example"
DOC_BLOCKS=(live-contract test-agreement verify-agreement)

# require_block FILE NAME
# Fail unless FILE contains exactly one `<!-- reset-testnet:begin NAME … -->`
# line followed (later) by exactly one `<!-- reset-testnet:end NAME -->` line.
require_block() {
  local file="$1" name="$2"
  if ! awk -v name="$name" '
    index($0, "<!-- reset-testnet:begin " name " ") == 1 { begins++; if (ends) bad = 1 }
    index($0, "<!-- reset-testnet:end " name " -->") == 1 { ends++; if (!begins) bad = 1 }
    END { exit !(begins == 1 && ends == 1 && !bad) }
  ' "$file"; then
    echo "error: $file must contain exactly one '<!-- reset-testnet:begin $name -->'" \
      "line followed by one '<!-- reset-testnet:end $name -->' line" >&2
    return 1
  fi
}

# replace_block FILE NAME CONTENT
# Replace everything between the begin/end marker lines for NAME in FILE with
# CONTENT. The marker lines themselves are kept so the block can be
# regenerated on the next reset.
replace_block() {
  local file="$1" name="$2" content="$3" tmp
  tmp=$(mktemp)
  BLOCK_CONTENT="$content" awk -v name="$name" '
    index($0, "<!-- reset-testnet:begin " name " ") == 1 {
      print; print ENVIRON["BLOCK_CONTENT"]; skipping = 1; next
    }
    index($0, "<!-- reset-testnet:end " name " -->") == 1 { skipping = 0 }
    !skipping { print }
  ' "$file" >"$tmp"
  cat "$tmp" >"$file"
  rm -f "$tmp"
}

# set_env_var FILE KEY VALUE
# Set KEY=VALUE in FILE, replacing an existing `KEY=` line (all other lines,
# comments and ordering are preserved) or appending one if KEY is absent.
set_env_var() {
  local file="$1" key="$2" value="$3" tmp
  tmp=$(mktemp)
  ENV_VALUE="$value" awk -v key="$key" '
    index($0, key "=") == 1 { print key "=" ENVIRON["ENV_VALUE"]; found = 1; next }
    { print }
    END { if (!found) print key "=" ENVIRON["ENV_VALUE"] }
  ' "$file" >"$tmp"
  cat "$tmp" >"$file"
  rm -f "$tmp"
}

# fail MESSAGE [DETAIL] — print a verification failure and exit non-zero.
fail() {
  echo "error: $1" >&2
  if [[ -n "${2:-}" ]]; then
    echo "$2" >&2
  fi
  exit 1
}

# Check the doc markers up front so a broken DEPLOYMENT.md fails fast, before
# spending several minutes building and deploying.
for block in "${DOC_BLOCKS[@]}"; do
  require_block "$DEPLOYMENT_DOC" "$block"
done

echo "==> Ensuring wasm32-unknown-unknown target is installed"
rustup target add wasm32-unknown-unknown

echo "==> Ensuring deployer identity exists and is funded"
if ! stellar keys address "$DEPLOYER_KEY" >/dev/null 2>&1; then
  stellar keys generate "$DEPLOYER_KEY" --network testnet
fi
stellar keys fund "$DEPLOYER_KEY" --network testnet --rpc-url "$RPC_URL"

echo "==> Building contract WASM"
cargo rustc \
  --manifest-path=contracts/trellis_core/Cargo.toml \
  --crate-type=cdylib \
  --target=wasm32-unknown-unknown \
  --release

WASM_PATH="target/wasm32-unknown-unknown/release/trellis_core.wasm"

echo "==> Deploying contract to testnet"
# stderr is captured (and replayed) so the deploy transaction hash can be
# linked from DEPLOYMENT.md.
DEPLOY_LOG=$(mktemp)
trap 'rm -f "$DEPLOY_LOG"' EXIT
if ! CONTRACT_ID=$(stellar contract deploy \
  --wasm "$WASM_PATH" \
  --source "$DEPLOYER_KEY" \
  --network testnet 2>"$DEPLOY_LOG"); then
  cat "$DEPLOY_LOG" >&2
  exit 1
fi
cat "$DEPLOY_LOG" >&2
DEPLOY_TX=$(grep -oiE '[0-9a-f]{64}' "$DEPLOY_LOG" | tail -n 1 || true)

if [[ ! "$CONTRACT_ID" =~ ^C[A-Z2-7]{55}$ ]]; then
  echo "error: unexpected contract ID from 'stellar contract deploy': '$CONTRACT_ID'" >&2
  exit 1
fi

echo "==> Building CLI"
cargo build --release --manifest-path=cli/trellis_cli/Cargo.toml

CLI_BIN="target/release/trellis"

echo "==> Creating payer/payee/resolver identities for the test agreement"
for name in trellis-payer trellis-payee trellis-resolver; do
  if ! stellar keys address "$name" >/dev/null 2>&1; then
    stellar keys generate "$name" --network testnet
  fi
  stellar keys fund "$name" --network testnet --rpc-url "$RPC_URL"
done

PAYER=$(stellar keys address trellis-payer)
PAYEE=$(stellar keys address trellis-payee)
RESOLVER=$(stellar keys address trellis-resolver)
AGREEMENT_ID=$(printf '01%.0s' {1..64} | head -c 64)
MILESTONE_AMOUNT=1000

# The agreement's token must be a token *contract* (C…), not an account or
# asset-issuer address (G…) — otherwise init succeeds but every lock_funds
# fails. Use the native XLM Stellar Asset Contract rather than USDC: Friendbot
# funds the payer with XLM only, so the lock_funds smoke test below can move
# real funds with no trustline or faucet setup.
echo "==> Resolving native XLM token contract (SAC) on testnet"
TOKEN_ID=$(stellar contract id asset --asset native --network testnet)
if [[ ! "$TOKEN_ID" =~ ^C[A-Z2-7]{55}$ ]]; then
  fail "unexpected token contract ID from 'stellar contract id asset': '$TOKEN_ID'"
fi

export STELLAR_RPC_URL="$RPC_URL"
export STELLAR_NETWORK_PASSPHRASE="$NETWORK_PASSPHRASE"
export TRELLIS_CONTRACT_ID="$CONTRACT_ID"
export TRELLIS_SOURCE_KEY="$DEPLOYER_KEY"

echo "==> Creating fresh test agreement"
"$CLI_BIN" init \
  --agreement-id "$AGREEMENT_ID" \
  --payer "$PAYER" \
  --payee "$PAYEE" \
  --token "$TOKEN_ID" \
  --resolver "$RESOLVER" \
  --milestones "$MILESTONE_AMOUNT" \
  --yes

echo "==> Verifying the agreement can be queried back from the chain"
AGREEMENT_OUT=$("$CLI_BIN" status --agreement-id "$AGREEMENT_ID") ||
  fail "'trellis status' could not read back agreement $AGREEMENT_ID"
for expected in "$PAYER" "$PAYEE" "$RESOLVER" "$TOKEN_ID"; do
  grep -q "$expected" <<<"$AGREEMENT_OUT" ||
    fail "agreement $AGREEMENT_ID does not reference $expected" "$AGREEMENT_OUT"
done

echo "==> Smoke test: locking milestone 0 funds as the payer"
TRELLIS_SOURCE_KEY=trellis-payer "$CLI_BIN" lock-funds \
  --agreement-id "$AGREEMENT_ID" \
  --milestone-id 0 \
  --yes ||
  fail "lock_funds failed against the freshly created agreement"

MILESTONE_OUT=$("$CLI_BIN" milestone-status --agreement-id "$AGREEMENT_ID" --milestone-id 0) ||
  fail "'trellis milestone-status' could not read back milestone 0"
grep -qw "Funded" <<<"$MILESTONE_OUT" ||
  fail "milestone 0 is not Funded after lock_funds" "$MILESTONE_OUT"

# The status flip alone doesn't prove tokens moved; check the escrow balance.
# The contract was deployed fresh above, so it must hold exactly one
# milestone's worth.
ESCROW_BALANCE=$(stellar contract invoke \
  --id "$TOKEN_ID" \
  --source "$DEPLOYER_KEY" \
  --network testnet \
  -- balance --id "$CONTRACT_ID" | tr -d '"[:space:]')
if [[ "$ESCROW_BALANCE" != "$MILESTONE_AMOUNT" ]]; then
  fail "escrow contract holds '$ESCROW_BALANCE' stroops after lock_funds, expected $MILESTONE_AMOUNT"
fi
echo "    agreement readable, milestone 0 Funded, escrow balance $ESCROW_BALANCE stroops"

if [[ -n "$DEPLOY_TX" ]]; then
  DEPLOY_TX_LINK="https://stellar.expert/explorer/testnet/tx/$DEPLOY_TX"
else
  # Hash not found in the deploy output (its format varies across stellar
  # CLI versions); the contract's explorer page lists its deploy transaction.
  DEPLOY_TX_LINK="https://stellar.expert/explorer/testnet/contract/$CONTRACT_ID"
fi

echo "==> Updating $DEPLOYMENT_DOC"
replace_block "$DEPLOYMENT_DOC" live-contract "| Network | Contract ID |
|---|---|
| Stellar Testnet | \`$CONTRACT_ID\` |

### Explorer Links

- **Contract:** https://lab.stellar.org/r/testnet/contract/$CONTRACT_ID
- **Deploy TX:** $DEPLOY_TX_LINK"

replace_block "$DEPLOYMENT_DOC" test-agreement "| Field | Value |
|---|---|
| Agreement ID | \`$AGREEMENT_ID\` |
| Payer | \`$PAYER\` |
| Payee | \`$PAYEE\` |
| Token | \`$TOKEN_ID\` (native XLM SAC) |
| Milestone 0 | $MILESTONE_AMOUNT stroops, status: Funded (locked by the reset smoke test) |"

replace_block "$DEPLOYMENT_DOC" verify-agreement "\`\`\`bash
trellis status --agreement-id $AGREEMENT_ID
\`\`\`"

echo "==> Updating $FRONTEND_ENV_FILE"
if [[ ! -f "$FRONTEND_ENV_FILE" ]]; then
  cp "$FRONTEND_ENV_EXAMPLE" "$FRONTEND_ENV_FILE"
fi
set_env_var "$FRONTEND_ENV_FILE" VITE_CONTRACT_ID "$CONTRACT_ID"

if [[ "$FRONTEND_ENV_FILE" != "$FRONTEND_ENV_EXAMPLE" ]]; then
  echo "==> Updating $FRONTEND_ENV_EXAMPLE"
  set_env_var "$FRONTEND_ENV_EXAMPLE" VITE_CONTRACT_ID "$CONTRACT_ID"
fi

cat <<EOF

==> Done. Updated $DEPLOYMENT_DOC, $FRONTEND_ENV_FILE and $FRONTEND_ENV_EXAMPLE
with the new values below. Review and commit them with:
  git diff $DEPLOYMENT_DOC $FRONTEND_ENV_EXAMPLE

Contract ID:   $CONTRACT_ID
Agreement ID:  $AGREEMENT_ID
Payer:         $PAYER
Payee:         $PAYEE
Resolver:      $RESOLVER
Token:         $TOKEN_ID (native XLM SAC)

Verified: agreement read back on-chain; milestone 0 funded via lock_funds
($ESCROW_BALANCE stroops held in escrow).

Verify with:
  TRELLIS_CONTRACT_ID=$CONTRACT_ID $CLI_BIN status --agreement-id $AGREEMENT_ID
EOF
