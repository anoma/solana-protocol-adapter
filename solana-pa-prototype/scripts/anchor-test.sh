#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=validator-deploy.sh
source "${SCRIPT_DIR}/validator-deploy.sh"

cd "$PROJECT_DIR"

# Phase selection, so CI can build once and fan the test phase out per mode:
#   all   (default) sync IDs, build, then validator + deploy + suite
#   build           sync IDs and build the programs, nothing else
#   test            validator + deploy + suite against existing artifacts
PHASE="${1:-all}"
if [[ "$PHASE" != "all" && "$PHASE" != "build" && "$PHASE" != "test" ]]; then
  echo "❌ phase must be 'all', 'build', or 'test', got '${PHASE}'" >&2
  exit 1
fi

# real: fixtures with Groth16 proofs, verified by the devnet-cloned verifier.
# mock: fixtures with mock seals, verified by the localnet mock verifier.
# Exported so the ts-mocha suite (tests/utils/fixtures.ts) sees it.
export PA_TEST_MODE="${PA_TEST_MODE:-real}"
validate_test_mode "$PA_TEST_MODE"
echo "==> Test mode: ${PA_TEST_MODE} (phase: ${PHASE})"

require_commands
ensure_wallet

if [[ "$PHASE" != "test" ]]; then
  echo "==> (1/3) Syncing program IDs and building"
  sync_program_ids
  build_programs_dev
else
  echo "==> (1/3) Syncing program IDs (using prebuilt artifacts)"
  sync_program_ids
  for keypair in "${PROGRAM_KEYPAIRS[@]}"; do
    binary="target/deploy/$(basename "$keypair" -keypair.json).so"
    if [[ ! -f "$binary" ]]; then
      echo "❌ phase 'test' needs prebuilt artifacts, but ${binary} is missing." >&2
      echo "   Run './scripts/anchor-test.sh build' first (or use the default phase)." >&2
      exit 1
    fi
  done
fi

if [[ "$PHASE" == "build" ]]; then
  echo "==> Build complete (skipping validator and tests)"
  exit 0
fi

check_required_fixture "$PA_TEST_MODE"

# Type-check every script and test against the generated program types. The
# suite's --type-check covers only the files it loads, and the operator
# scripts are run by nothing else, so a script that no longer compiles would
# only surface when an operator runs it on a live cluster.
echo "==> Type-checking scripts and tests"
yarn run tsc --noEmit -p ./tsconfig.json

trap 'stop_validator' EXIT

echo "==> (2/3) Starting validator"

# Kill any stale validator from a previous interrupted run
if pkill -f solana-test-validator 2>/dev/null; then
  sleep 1
fi

start_validator

echo "==> (3/3) Deploying and running tests"
deploy_programs

ANCHOR_PROVIDER_URL="$CLUSTER_URL" \
ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
  yarn run ts-mocha --type-check -p ./tsconfig.json -t 1000000 'tests/**/*.ts'

if [[ "$PHASE" != "test" ]]; then
  # Anchor's SBF toolchain can leave incompatible host debug artifacts in
  # target/. Clean host packages so subsequent nix cargo commands always
  # rebuild with nix rustc. (The 'test' phase compiles nothing, and on CI
  # its runner has no cargo.)
  cargo clean \
    --package block-time-forwarder \
    --package spl-token-forwarder \
    --package protocol-adapter \
    >/dev/null 2>&1 || true
fi

echo "==> All tests passed"
