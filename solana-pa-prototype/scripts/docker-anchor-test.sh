#!/usr/bin/env bash
set -euo pipefail

# Deterministic Anchor test runner with two-build strategy.
#
# Why two builds:
# - Default build (no feature): aggregated path works, non-aggregated returns AggregationRequired
# - Feature build (--features non-aggregated-proofs): both paths work
#
# Why recreate validator: the validator runs with a persisted ledger volume;
# if it stays up between runs, previously-spent nullifiers can cause test
# flakiness. Recreating ensures `solana-test-validator --reset` runs.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

CLUSTER_URL="${CLUSTER_URL:-http://solana-validator:8899}"

cd "$PROJECT_DIR"

# =============================================================================
# Step 1: Sync program IDs and build (only rebuilds if IDs changed)
# =============================================================================
echo "==> (1/5) Syncing program IDs and building"
docker compose run --rm dev bash -lc '
  cd /workspace/solana-pa-prototype

  # Install node dependencies (needed for fixture validation)
  # Try frozen-lockfile first; if it fails, regenerate lockfile
  if ! yarn install --frozen-lockfile 2>/dev/null; then
    echo "    Lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi

  # Install risc0-solana dependencies (imported by setup.ts and tests)
  echo "    Installing risc0-solana dependencies..."
  cd /workspace/risc0-solana/solana-verifier
  if ! yarn install --frozen-lockfile 2>/dev/null; then
    echo "    risc0-solana lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi
  cd /workspace/solana-pa-prototype

  NEEDS_BUILD=false

  # Build to generate keypairs if they don'\''t exist
  if [[ ! -f "target/deploy/solana_pa_prototype-keypair.json" ]] || \
     [[ ! -f "target/deploy/block_time_forwarder-keypair.json" ]]; then
    echo "    Generating keypairs..."
    anchor build
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

  # Always rebuild PA without features to ensure default build for two-build strategy.
  # Step 4 will rebuild with features.
  echo "    Building PA (default, no features)..."
  anchor build -p solana-pa-prototype

  # Build block_time_forwarder if needed
  if [[ "$NEEDS_BUILD" == "true" ]] || [[ ! -f "target/deploy/block_time_forwarder.so" ]]; then
    echo "    Building block_time_forwarder..."
    anchor build -p block-time-forwarder
  fi

  # Fixture definitions: path|flags
  FIXTURES=(
    "tests/fixtures/batch_groth16.json|"
    "tests/fixtures/individual_groth16.json|--non-aggregated"
    "tests/fixtures/batch_groth16_mismatch.json|--output-mismatch"
  )

  # Build fixture-gen once if needed
  (cd tools/fixture-gen && cargo build --release)

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
echo "==> (2/5) Starting validator"
docker compose --profile validator up -d --force-recreate validator

echo "    Waiting for RPC health..."
for i in {1..60}; do
  if curl -fsS "http://localhost:8899/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "http://localhost:8899/health" >/dev/null

# =============================================================================
# Step 3: Run tests (default build - non-aggregated should fail)
# =============================================================================
echo "==> (3/5) Running tests (default build)"
docker compose run --rm dev bash -lc "
  cd /workspace/solana-pa-prototype
  set +e
  anchor test --skip-local-validator --skip-build --provider.cluster '${CLUSTER_URL}' 2>&1 | tee /tmp/test-output.txt
  exit_code=\${PIPESTATUS[0]}

  # Check if the only failures are the expected AggregationRequired errors
  if [ \$exit_code -ne 0 ]; then
    failure_count=\$(grep -oP '\\d+(?= failing)' /tmp/test-output.txt || echo 0)
    aggregation_errors=\$(grep -ci 'aggregation.*required' /tmp/test-output.txt || echo 0)

    if [ \"\$failure_count\" = \"2\" ] && [ \"\$aggregation_errors\" -ge 2 ]; then
      echo ''
      echo '==> Expected failures: Non-aggregated tests correctly rejected with AggregationRequired'
      echo '==> Default build verification PASSED'
    else
      echo ''
      echo '==> UNEXPECTED FAILURES: expected 2 failures with AggregationRequired'
      echo \"==> Got \$failure_count failures and \$aggregation_errors AggregationRequired errors\"
      exit 1
    fi
  else
    echo ''
    echo '==> All tests passed (this is unexpected for default build)'
    echo '==> Non-aggregated tests should have failed with AggregationRequired'
    exit 1
  fi
"

# =============================================================================
# Step 4: Rebuild with non-aggregated feature
# =============================================================================
echo "==> (4/5) Rebuilding PA with non-aggregated-proofs feature"
docker compose run --rm dev bash -lc "cd /workspace/solana-pa-prototype && anchor build -p solana-pa-prototype -- --features non-aggregated-proofs"

# =============================================================================
# Step 5: Reset validator and run all tests
# =============================================================================
echo "==> (5/5) Resetting validator and running all tests"
docker compose --profile validator up -d --force-recreate validator

for i in {1..60}; do
  if curl -fsS "http://localhost:8899/health" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
curl -fsS "http://localhost:8899/health" >/dev/null

docker compose run --rm dev bash -lc "
  cd /workspace/solana-pa-prototype
  anchor test --skip-local-validator --skip-build --provider.cluster '${CLUSTER_URL}'
"

echo "==> All tests passed"
