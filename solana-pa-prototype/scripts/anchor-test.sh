#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

CLUSTER_URL="${CLUSTER_URL:-http://127.0.0.1:8899}"
VALIDATOR_LEDGER="${VALIDATOR_LEDGER:-${PROJECT_DIR}/.validator-ledger}"
VALIDATOR_LOG="${VALIDATOR_LOG:-${PROJECT_DIR}/.validator.log}"
ANCHOR_WALLET_PATH="${ANCHOR_WALLET:-$HOME/.config/solana/id.json}"

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

fixture_matches_program_id() {
  local fixture_path="$1"
  local program_id="$2"

  node -e '
    const fs = require("fs");
    const bs58 = require("bs58").default || require("bs58");

    const fixturePath = process.argv[1];
    const programId = process.argv[2];

    const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf-8"));
    if (!fixture.selector || !fixture.tx_b64) {
      process.exit(1);
    }

    const txBytes = Buffer.from(fixture.tx_b64, "base64");
    const programIdBytes = Buffer.from(bs58.decode(programId));
    process.exit(txBytes.includes(programIdBytes) ? 0 : 1);
  ' "$fixture_path" "$program_id"
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

cleanup() {
  if [[ -n "${VALIDATOR_PID:-}" ]] && kill -0 "$VALIDATOR_PID" >/dev/null 2>&1; then
    kill "$VALIDATOR_PID" >/dev/null 2>&1 || true
    wait "$VALIDATOR_PID" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

require_cmd anchor
require_cmd solana
require_cmd solana-test-validator
require_cmd cargo
require_cmd node
require_cmd yarn
require_cmd curl

ensure_wallet

cd "$PROJECT_DIR"

echo "==> (1/3) Syncing program IDs and building"

if ! yarn install --frozen-lockfile; then
  echo "    Lockfile out of sync, regenerating..."
  rm -f yarn.lock package-lock.json
  yarn install
fi

if [[ ! -f "target/deploy/solana_pa_prototype-keypair.json" ]] || [[ ! -f "target/deploy/block_time_forwarder-keypair.json" ]]; then
  echo "    Generating missing program keypairs..."
  build_with_filtered_output anchor build --no-idl
fi

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
  sed -i -E "s/decode_base58_32\(\"[^\"]+\"\)/decode_base58_32(\"${BTF_ID}\")/" tools/fixture-gen/src/main.rs
  sed -i -E "s/blockTimeForwarderId = new PublicKey\(\"[^\"]+\"\)/blockTimeForwarderId = new PublicKey(\"${BTF_ID}\")/" tests/solana-pa-prototype.ts
fi

# anchor build uses cargo +nightly for IDL generation, which is incompatible
# with debug artifacts compiled by the stable toolchain (e.g. from cargo test).
rm -rf target/debug/

echo "    Building programs..."
build_with_filtered_output anchor build

REQUIRED_FIXTURE="tests/fixtures/batch_groth16.json"
OPTIONAL_MISMATCH_FIXTURE="tests/fixtures/batch_groth16_mismatch.json"

if [[ ! -f "$REQUIRED_FIXTURE" ]] || ! fixture_matches_program_id "$REQUIRED_FIXTURE" "$BTF_ID"; then
  echo "Required fixture is missing or stale: ${REQUIRED_FIXTURE}"
  echo "Regenerate it with:"
  echo "  cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- tests/fixtures/batch_groth16.json"
  exit 1
fi

if [[ -f "$OPTIONAL_MISMATCH_FIXTURE" ]] && ! fixture_matches_program_id "$OPTIONAL_MISMATCH_FIXTURE" "$BTF_ID"; then
  echo "Warning: ${OPTIONAL_MISMATCH_FIXTURE} is stale for block_time_forwarder ID ${BTF_ID}."
  echo "Regenerate it with:"
  echo "  cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- --output-mismatch tests/fixtures/batch_groth16_mismatch.json"
fi

echo "==> (2/3) Starting validator"
mkdir -p "$VALIDATOR_LEDGER"

# RISC0 verifier programs and PDAs cloned from devnet
VERIFIER_ROUTER="BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg"
GROTH16_VERIFIER="2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD"
ROUTER_PDA="9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S"
VERIFIER_ENTRY_PDA="4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey"

solana-test-validator \
  --reset \
  --ledger "$VALIDATOR_LEDGER" \
  --rpc-port 8899 \
  --faucet-port 9900 \
  --bind-address 127.0.0.1 \
  --url devnet \
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
  exit 1
fi

echo "==> (3/3) Deploying and running tests"
anchor deploy --provider.cluster "$CLUSTER_URL"

ANCHOR_PROVIDER_URL="$CLUSTER_URL" \
ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
  yarn run ts-mocha -p ./tsconfig.json -t 1000000 'tests/**/*.ts'

echo "==> All tests passed"
