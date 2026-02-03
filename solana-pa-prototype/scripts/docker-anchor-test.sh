#!/usr/bin/env bash
set -euo pipefail

# Deterministic Anchor test runner.
#
# Why recreate validator: the validator runs with a persisted ledger volume;
# if it stays up between runs, previously-spent nullifiers can cause test
# flakiness. Recreating ensures `solana-test-validator --reset` runs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

CLUSTER_URL="${CLUSTER_URL:-http://solana-validator:8899}"

# Build compose command: add GPU override when USE_GPU=1
COMPOSE="docker compose"
if [[ "${USE_GPU:-}" == "1" ]]; then
  COMPOSE="docker compose -f docker-compose.yml -f docker-compose.gpu.yml"
fi

cd "$PROJECT_DIR"

# =============================================================================
# Step 1: Sync program IDs and build (only rebuilds if IDs changed)
# =============================================================================
echo "==> (1/3) Syncing program IDs and building"
$COMPOSE run --rm dev bash -lc '
  cd /workspace/solana-pa-prototype

  # Install node dependencies (needed for fixture validation)
  # Try frozen-lockfile first; if it fails, regenerate lockfile
  if ! yarn install --frozen-lockfile 2>/dev/null; then
    echo "    Lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi

  NEEDS_BUILD=false

  # Build to generate keypairs if they don'\''t exist
  if [[ ! -f "target/deploy/solana_pa_prototype-keypair.json" ]] || \
     [[ ! -f "target/deploy/block_time_forwarder-keypair.json" ]]; then
    echo "    Generating keypairs..."
    anchor build 2>&1 | grep -v "^warning:\|^ *-->\|^ *[0-9]* |\|^ *|\|^ *=\|generated [0-9]* warning\|future-incompat-report" | cat -s
    NEEDS_BUILD=false  # Already built
  fi

  # Sync solana_pa_prototype program ID
  PA_KEYPAIR="target/deploy/solana_pa_prototype-keypair.json"
  PA_ID="$(solana-keygen pubkey "$PA_KEYPAIR")"
  PA_CURRENT=$(grep -oP "declare_id!\(\"\K[^\"]+(?=\"\))" programs/solana-pa-prototype/src/lib.rs || echo "")

  if [[ "$PA_CURRENT" != "$PA_ID" ]]; then
    echo "    PA program ID mismatch: $PA_CURRENT -> $PA_ID"
    sed -i -E "s/^declare_id!\(\"[^\"]+\"\);/declare_id!(\"${PA_ID}\");/" \
      programs/solana-pa-prototype/src/lib.rs
    sed -i -E "s/^solana_pa_prototype = \"[^\"]+\"$/solana_pa_prototype = \"${PA_ID}\"/" \
      Anchor.toml
    NEEDS_BUILD=true
  else
    echo "    PA program ID already synced: $PA_ID"
  fi

  # Sync block_time_forwarder program ID
  BTF_KEYPAIR="target/deploy/block_time_forwarder-keypair.json"
  BTF_ID="$(solana-keygen pubkey "$BTF_KEYPAIR")"
  BTF_CURRENT=$(grep -oP "declare_id!\(\"\K[^\"]+(?=\"\))" programs/block-time-forwarder/src/lib.rs || echo "")

  if [[ "$BTF_CURRENT" != "$BTF_ID" ]]; then
    echo "    block_time_forwarder program ID mismatch: $BTF_CURRENT -> $BTF_ID"

    # Update lib.rs
    sed -i -E "s/^declare_id!\(\"[^\"]+\"\);/declare_id!(\"${BTF_ID}\");/" \
      programs/block-time-forwarder/src/lib.rs

    # Update Anchor.toml
    sed -i -E "s/^block_time_forwarder = \"[^\"]+\"$/block_time_forwarder = \"${BTF_ID}\"/" \
      Anchor.toml

    # Update fixture-gen (hardcoded program ID)
    sed -i -E "s/decode_base58_32\(\"[^\"]+\"\)/decode_base58_32(\"${BTF_ID}\")/" \
      tools/fixture-gen/src/main.rs

    # Update test file (hardcoded program ID)
    sed -i -E "s/blockTimeForwarderId = new PublicKey\(\"[^\"]+\"\)/blockTimeForwarderId = new PublicKey(\"${BTF_ID}\")/" \
      tests/solana-pa-prototype.ts

    NEEDS_BUILD=true
  else
    echo "    block_time_forwarder program ID already synced: $BTF_ID"
  fi

  # Build PA
  echo "    Building PA..."
  anchor build -p solana-pa-prototype 2>&1 | grep -v "^warning:\|^ *-->\|^ *[0-9]* |\|^ *|\|^ *=\|generated [0-9]* warning\|future-incompat-report" | cat -s

  # Build block_time_forwarder if needed
  if [[ "$NEEDS_BUILD" == "true" ]] || [[ ! -f "target/deploy/block_time_forwarder.so" ]]; then
    echo "    Building block_time_forwarder..."
    anchor build -p block-time-forwarder 2>&1 | grep -v "^warning:\|^ *-->\|^ *[0-9]* |\|^ *|\|^ *=\|generated [0-9]* warning\|future-incompat-report" | cat -s
  fi

  # Fixture definitions: path|flags
  FIXTURES=(
    "tests/fixtures/batch_groth16.json|"
    "tests/fixtures/batch_groth16_mismatch.json|--output-mismatch"
  )

  # Build fixture-gen once if needed
  (cd tools/fixture-gen && cargo build --release 2>&1 | grep -v "^warning:\|^ *-->\|^ *[0-9]* |\|^ *|\|^ *=\|generated [0-9]* warning\|future-incompat-report" | cat -s)

  # Check and regenerate each fixture if invalid
  for entry in "${FIXTURES[@]}"; do
    IFS="|" read -r path flags <<< "$entry"
    if [[ ! -f "$path" ]] || ! node -e "
      const bs58 = require(\"bs58\").default || require(\"bs58\");
      const fs = require(\"fs\");
      const fixture = JSON.parse(fs.readFileSync(\"$path\"));
      if (!fixture.selector) process.exit(1);
      const txBytes = Buffer.from(fixture.tx_b64, \"base64\");
      const programIdBytes = Buffer.from(bs58.decode(\"$BTF_ID\"));
      process.exit(txBytes.includes(programIdBytes) ? 0 : 1);
    " 2>/dev/null; then
      echo "    Generating $path (~30 min)..."
      ./tools/fixture-gen/target/release/fixture-gen --threads 6 $flags "$path"
    fi
  done
'

# =============================================================================
# Step 2: Start validator
# =============================================================================
echo "==> (2/3) Starting validator"
$COMPOSE --profile validator up -d --force-recreate validator

echo "    Waiting for RPC health..."
for i in {1..60}; do
  if curl -fsS "http://localhost:8899/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "http://localhost:8899/health" >/dev/null

# =============================================================================
# Step 3: Run tests
# =============================================================================
echo "==> (3/3) Running tests"
$COMPOSE run --rm dev bash -lc "
  cd /workspace/solana-pa-prototype
  anchor test --skip-local-validator --skip-build --provider.cluster '${CLUSTER_URL}'
"

echo "==> All tests passed"
