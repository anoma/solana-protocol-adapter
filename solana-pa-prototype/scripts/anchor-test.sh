#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=validator-deploy.sh
source "${SCRIPT_DIR}/validator-deploy.sh"

trap 'stop_validator' EXIT

cd "$PROJECT_DIR"

require_commands
ensure_wallet

echo "==> (1/3) Syncing program IDs and building"
sync_program_ids
build_programs

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

# Anchor's SBF toolchain can leave incompatible host debug artifacts in target/.
# Clean host packages so subsequent nix cargo commands always rebuild with nix rustc.
cargo clean \
  --package block-time-forwarder \
  --package solana-pa-prototype \
  >/dev/null 2>&1 || true

echo "==> All tests passed"
