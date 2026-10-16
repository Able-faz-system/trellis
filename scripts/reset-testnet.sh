#!/usr/bin/env bash
# Redeploys the Trellis contract to Stellar testnet and creates a fresh test
# agreement. Use this after a testnet reset invalidates the contract ID or
# agreement ID referenced in DEPLOYMENT.md.
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
USDC_ISSUER="GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5"
# `init` needs the USDC Stellar Asset Contract (C...) address, not the
# issuer account (G...) — derive it from the asset.
USDC_SAC=$(stellar contract id asset --network testnet --asset "USDC:$USDC_ISSUER")
AGREEMENT_ID=$(printf '01%.0s' {1..64} | head -c 64)

export STELLAR_RPC_URL="$RPC_URL"
export STELLAR_NETWORK_PASSPHRASE="$NETWORK_PASSPHRASE"
export TRELLIS_CONTRACT_ID="$CONTRACT_ID"
export TRELLIS_SOURCE_KEY="$DEPLOYER_KEY"

echo "==> Creating fresh test agreement"
"$CLI_BIN" init \
  --agreement-id "$AGREEMENT_ID" \
  --payer "$PAYER" \
  --payee "$PAYEE" \
  --token "$USDC_SAC" \
  --resolver "$RESOLVER" \
  --milestones "1000"

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
| Milestone 0 | 1000 stroops, status: Pending |"

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

Verify with:
  TRELLIS_CONTRACT_ID=$CONTRACT_ID $CLI_BIN status --agreement-id $AGREEMENT_ID
EOF
