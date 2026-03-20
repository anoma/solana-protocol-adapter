#!/usr/bin/env bash
# Shared validator deployment functions.
# Source this file from other scripts; do not execute directly.
#
# Required variables (set by caller before sourcing):
#   PROJECT_DIR     - path to solana-pa-prototype
#
# Optional variables:
#   CLUSTER_URL          - RPC URL (default: http://127.0.0.1:8899)
#   VALIDATOR_LEDGER     - ledger directory (default: $PROJECT_DIR/.validator-ledger)
#   VALIDATOR_LOG        - log file (default: $PROJECT_DIR/.validator.log)
#   ANCHOR_WALLET_PATH   - wallet path (default: ~/.config/solana/id.json)
#
# Exported after sync_program_ids:
#   PA_ID, BTF_ID, TF_ID, STF_ID
#
# Exported after start_validator:
#   VALIDATOR_PID

: "${PROJECT_DIR:=$(pwd)}"

CLUSTER_URL="${CLUSTER_URL:-http://127.0.0.1:8899}"
VALIDATOR_LEDGER="${VALIDATOR_LEDGER:-${PROJECT_DIR}/.validator-ledger}"
VALIDATOR_LOG="${VALIDATOR_LOG:-${PROJECT_DIR}/.validator.log}"
ANCHOR_WALLET_PATH="${ANCHOR_WALLET:-$HOME/.config/solana/id.json}"

# RISC0 verifier programs and PDAs cloned from devnet
VERIFIER_ROUTER="595BRy1i9be8TWQNpmd4jUcaRzrQ9JV3fCM4efvrN8xG"
GROTH16_VERIFIER="5oU8BusYfNSQyiDk26KGa54oBQSXACHcT9fsbLvxrS8W"
ROUTER_PDA="7UwV87rvuhF1hFGdRKh8tsTKP5to6q4mavSiDHM6aeEf"
VERIFIER_ENTRY_PDA="4egZC2TLC5tqd6h6tZyCkmbH5GWPrKxeuicyyfULnXLv"

VALIDATOR_PID=""

# ── Helpers ────────────────────────────────────────────────────────────

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1"
    echo "Run this inside the Nix dev shell: nix develop"
    exit 1
  fi
}

read_declare_id() {
  sed -n 's/^declare_id!("\([^"]*\)").*/\1/p' "$1" | head -n 1
}

ensure_wallet() {
  mkdir -p "$(dirname "$ANCHOR_WALLET_PATH")"
  if [[ ! -f "$ANCHOR_WALLET_PATH" ]]; then
    echo "Generating Solana keypair..."
    solana-keygen new --no-bip39-passphrase -o "$ANCHOR_WALLET_PATH"
  fi
}

build_with_filtered_output() {
  "$@" 2>&1 | grep -v '^warning:\|^ *-->\|^ *[0-9]* |\|^ *|\|^ *=\|generated [0-9]* warning\|future-incompat-report' | cat -s
}

fixture_matches_requirements() {
  local fixture_path="$1"
  local program_id="$2"

  cargo run --locked --manifest-path tools/fixture-gen/Cargo.toml -- \
    --validate "$fixture_path" \
    --program-id "$program_id" \
    >/dev/null 2>&1
}

print_fixture_validation_error() {
  local fixture_path="$1"
  local program_id="$2"

  cargo run --locked --manifest-path tools/fixture-gen/Cargo.toml -- \
    --validate "$fixture_path" \
    --program-id "$program_id"
}

regenerate_fixture() {
  local fixture_path="$1"
  shift

  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "Docker is required to regenerate stale fixtures."
    echo "Ensure Docker is installed/running, then rerun."
    exit 1
  fi

  local threads="${FIXTURE_THREADS:-6}"
  echo "    Regenerating fixture: ${fixture_path}"
  cargo run --release --locked --manifest-path tools/fixture-gen/Cargo.toml -- --threads "$threads" "$@" "$fixture_path"
}

ensure_fixture_matches_program() {
  local fixture_path="$1"
  local program_id="$2"
  local label="$3"
  shift 3

  if [[ ! -f "$fixture_path" ]] || ! fixture_matches_requirements "$fixture_path" "$program_id"; then
    echo "    ${label} fixture is missing or stale."
    regenerate_fixture "$fixture_path" "$@"
  fi

  if ! fixture_matches_requirements "$fixture_path" "$program_id"; then
    echo "    ${label} fixture does not match program ID ${program_id}: ${fixture_path}"
    print_fixture_validation_error "$fixture_path" "$program_id" || true
    exit 1
  fi
}

wait_for_validator() {
  local url="$1"
  local attempts=60

  for _ in $(seq 1 "$attempts"); do
    if curl -fsS "${url}/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  return 1
}

# ── Sync helpers ───────────────────────────────────────────────────────

sync_program_id() {
  local name="$1"
  local keypair="$2"
  local lib_rs="$3"
  local anchor_key="$4"

  local id
  id="$(solana-keygen pubkey "$keypair")"
  local current
  current="$(read_declare_id "$lib_rs")"

  if [[ "$current" != "$id" ]]; then
    echo "    ${name} program ID mismatch: $current -> $id" >&2
    sed -i -E "s/^declare_id!\(\"[^\"]+\"\);/declare_id!(\"${id}\");/" "$lib_rs"
    sed -i -E "s/^${anchor_key} = \"[^\"]+\"$/${anchor_key} = \"${id}\"/" Anchor.toml
  else
    echo "    ${name} program ID already synced: $id" >&2
  fi

  printf '%s' "$id"
}

# ── Main functions ─────────────────────────────────────────────────────

require_commands() {
  require_cmd anchor
  require_cmd solana
  require_cmd solana-test-validator
  require_cmd cargo
  require_cmd node
  require_cmd yarn
  require_cmd curl
}

sync_program_ids() {
  if ! yarn install --frozen-lockfile; then
    echo "    Lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi

  if [[ ! -f "target/deploy/solana_pa_prototype-keypair.json" ]] || \
     [[ ! -f "target/deploy/block_time_forwarder-keypair.json" ]] || \
     [[ ! -f "target/deploy/test_forwarder-keypair.json" ]]; then
    echo "    Generating missing program keypairs..."
    build_with_filtered_output anchor build --no-idl
  fi

  PA_ID="$(sync_program_id "PA" \
    "target/deploy/solana_pa_prototype-keypair.json" \
    "programs/solana-pa-prototype/src/lib.rs" \
    "solana_pa_prototype")"

  BTF_OLD="$(read_declare_id "programs/block-time-forwarder/src/lib.rs")"
  BTF_ID="$(sync_program_id "block_time_forwarder" \
    "target/deploy/block_time_forwarder-keypair.json" \
    "programs/block-time-forwarder/src/lib.rs" \
    "block_time_forwarder")"

  # BTF ID also appears in fixture-gen and integration tests
  if [[ "$BTF_OLD" != "$BTF_ID" ]]; then
    sed -i -E "s/decode_base58_32\(\"${BTF_OLD}\"\)/decode_base58_32(\"${BTF_ID}\")/" tools/fixture-gen/src/main.rs
    sed -i -E "s/blockTimeForwarderId = new PublicKey\(\"[^\"]+\"\)/blockTimeForwarderId = new PublicKey(\"${BTF_ID}\")/" tests/solana-pa-prototype.ts
  fi

  TF_OLD="$(read_declare_id "programs/test-forwarder/src/lib.rs")"
  TF_ID="$(sync_program_id "test_forwarder" \
    "target/deploy/test_forwarder-keypair.json" \
    "programs/test-forwarder/src/lib.rs" \
    "test_forwarder")"

  STF_OLD="$(read_declare_id "programs/spl-token-forwarder/src/lib.rs")"
  STF_ID="$(sync_program_id "spl_token_forwarder" \
    "target/deploy/spl_token_forwarder-keypair.json" \
    "programs/spl-token-forwarder/src/lib.rs" \
    "spl_token_forwarder")"

  # TF ID also appears in fixture-gen and integration tests
  if [[ "$TF_OLD" != "$TF_ID" ]]; then
    sed -i -E "s/decode_base58_32\(\"${TF_OLD}\"\)/decode_base58_32(\"${TF_ID}\")/" tools/fixture-gen/src/main.rs
    sed -i -E "s/testForwarderId = new PublicKey\(\"[^\"]+\"\)/testForwarderId = new PublicKey(\"${TF_ID}\")/" tests/solana-pa-prototype.ts
  fi

  # STF ID also appears in fixture-gen and test constants — always sync to keypair
  sed -i -E "s/SPL_TOKEN_FORWARDER_PROGRAM_ID: \&str = \"[^\"]+\"/SPL_TOKEN_FORWARDER_PROGRAM_ID: \&str = \"${STF_ID}\"/" tools/fixture-gen/src/main.rs
  sed -i -E "s/SPL_TOKEN_FORWARDER_PROGRAM_ID = new PublicKey\(\"[^\"]+\"\)/SPL_TOKEN_FORWARDER_PROGRAM_ID = new PublicKey(\"${STF_ID}\")/" tests/utils/constants.ts
}

build_programs() {
  # anchor build uses cargo +nightly for IDL generation, which is incompatible
  # with debug artifacts compiled by the stable toolchain (e.g. from cargo test).
  # Remove incremental build state and proc-macro artifacts to avoid ABI mismatch.
  rm -rf target/debug/incremental target/debug/build

  echo "    Building programs..."
  build_with_filtered_output anchor build

  # Validate and regenerate all fixtures that embed program IDs.
  # Block-time-forwarder fixtures (BTF_ID)
  ensure_fixture_matches_program "tests/fixtures/batch_groth16.json" "$BTF_ID" "Batch Groth16"
  if [[ -f "tests/fixtures/batch_groth16_mismatch.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_groth16_mismatch.json" "$BTF_ID" "Batch Groth16 mismatch" "--output-mismatch"
  fi
  if [[ -f "tests/fixtures/batch_groth16_v2.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_groth16_v2.json" "$BTF_ID" "Batch Groth16 v2" "--nonce-seed" "3"
  fi
  if [[ -f "tests/fixtures/batch_groth16_v3.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_groth16_v3.json" "$BTF_ID" "Batch Groth16 v3" "--nonce-seed" "4"
  fi
  if [[ -f "tests/fixtures/batch_groth16_multi_call.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_groth16_multi_call.json" "$BTF_ID" "Batch Groth16 multi-call" "--nonce-seed" "5" "--multi-external-call"
  fi

  # Test-forwarder fixtures (TF_ID)
  if [[ -f "tests/fixtures/batch_forwarder_fail.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_forwarder_fail.json" "$TF_ID" "Forwarder fail" "--nonce-seed" "6" "--forwarder-fail"
  fi
  if [[ -f "tests/fixtures/batch_forwarder_silent.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_forwarder_silent.json" "$TF_ID" "Forwarder silent" "--nonce-seed" "7" "--forwarder-silent"
  fi
  if [[ -f "tests/fixtures/batch_forwarder_output.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/batch_forwarder_output.json" "$TF_ID" "Forwarder output" "--nonce-seed" "8" "--forwarder-output-account"
  fi

  # SPL Token Forwarder fixtures (STF_ID)
  if [[ -f "tests/fixtures/spl_token_wrap.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/spl_token_wrap.json" "$STF_ID" "SPL wrap" "--spl-token-wrap"
  fi
  if [[ -f "tests/fixtures/spl_token_unwrap.json" ]]; then
    ensure_fixture_matches_program "tests/fixtures/spl_token_unwrap.json" "$STF_ID" "SPL unwrap" "--spl-token-unwrap"
  fi
}

start_validator() {
  # Kill any existing validator on the target port to avoid bind conflicts
  local existing_pid
  existing_pid=$(lsof -ti :8899 2>/dev/null || true)
  if [[ -n "$existing_pid" ]]; then
    echo "Killing existing process on port 8899 (pid $existing_pid)"
    kill -9 $existing_pid 2>/dev/null || true
    sleep 1
  fi

  mkdir -p "$VALIDATOR_LEDGER"

  solana-test-validator \
    --reset \
    --ledger "$VALIDATOR_LEDGER" \
    --rpc-port 8899 \
    --faucet-port 9900 \
    --bind-address 127.0.0.1 \
    --url devnet \
    --bpf-program "$STF_ID" "target/deploy/spl_token_forwarder.so" \
    --clone-upgradeable-program "$VERIFIER_ROUTER" \
    --clone-upgradeable-program "$GROTH16_VERIFIER" \
    --clone "$ROUTER_PDA" \
    --clone "$VERIFIER_ENTRY_PDA" \
    --log \
    >"$VALIDATOR_LOG" 2>&1 &
  VALIDATOR_PID=$!

  if ! wait_for_validator "$CLUSTER_URL"; then
    echo "Validator failed to start. Last lines from ${VALIDATOR_LOG}:"
    tail -n 50 "$VALIDATOR_LOG" || true
    return 1
  fi
}

deploy_programs() {
  anchor deploy --provider.cluster "$CLUSTER_URL" --program-name solana_pa_prototype
  anchor deploy --provider.cluster "$CLUSTER_URL" --program-name block_time_forwarder
  anchor deploy --provider.cluster "$CLUSTER_URL" --program-name test_forwarder
}

stop_validator() {
  if [[ -n "${VALIDATOR_PID:-}" ]] && kill -0 "$VALIDATOR_PID" >/dev/null 2>&1; then
    kill "$VALIDATOR_PID" >/dev/null 2>&1 || true
    wait "$VALIDATOR_PID" >/dev/null 2>&1 || true
  fi
  VALIDATOR_PID=""
}
