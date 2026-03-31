#!/usr/bin/env bash
set -euo pipefail

# Run security/exploit integration tests.
# Uses the same validator setup as anchor-test.sh but only runs
# the setup and security test files.

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

if pkill -f solana-test-validator 2>/dev/null; then
  sleep 1
fi

start_validator

echo "==> (3/3) Deploying and running security tests"
deploy_programs

ANCHOR_PROVIDER_URL="$CLUSTER_URL" \
ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
  yarn run ts-mocha -p ./tsconfig.json -t 1000000 \
  'tests/exploit-*.ts'

echo "==> Security tests complete"
