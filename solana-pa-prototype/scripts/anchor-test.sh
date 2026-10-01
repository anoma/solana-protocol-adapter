#!/usr/bin/env bash
# Local integration flow. Every spec file runs against its own validator
# started on a fresh ledger, so no file depends on another's on-chain state
# or on the order files run in.
#
#   anchor-test.sh [all|build|test] [spec file...]
#
# Phase selection, so CI can build once and fan the test phase out per mode:
#   all   (default) sync IDs, build, then the spec files
#   build           sync IDs and build the programs, nothing else
#   test            the spec files against existing artifacts
# Spec files default to every tests/**/*.ts outside tests/utils/ (the
# support modules the specs import), in sorted order.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=validator-deploy.sh
source "${SCRIPT_DIR}/validator-deploy.sh"

cd "$PROJECT_DIR"

PHASE="${1:-all}"
if [[ "$PHASE" != "all" && "$PHASE" != "build" && "$PHASE" != "test" ]]; then
  echo "❌ phase must be 'all', 'build', or 'test', got '${PHASE}'" >&2
  exit 1
fi
if [[ $# -gt 0 ]]; then
  shift
fi
SPEC_FILES=("$@")
if [[ "$PHASE" == "build" && ${#SPEC_FILES[@]} -gt 0 ]]; then
  echo "❌ phase 'build' runs no tests; drop the spec file arguments" >&2
  exit 1
fi
if [[ ${#SPEC_FILES[@]} -eq 0 ]]; then
  mapfile -t SPEC_FILES < <(find tests -name '*.ts' -not -path 'tests/utils/*' | LC_ALL=C sort)
fi
for spec in "${SPEC_FILES[@]}"; do
  if [[ ! -f "$spec" ]]; then
    echo "❌ spec file not found: ${spec} (paths are relative to ${PROJECT_DIR})" >&2
    exit 1
  fi
done

# real: fixtures with Groth16 proofs, verified by the devnet-cloned verifier.
# mock: fixtures with mock seals, verified by the localnet mock verifier.
# Exported so the ts-mocha suite (tests/utils/fixtures.ts) sees it.
export PA_TEST_MODE="${PA_TEST_MODE:-real}"
validate_test_mode "$PA_TEST_MODE"
echo "==> Test mode: ${PA_TEST_MODE} (phase: ${PHASE})"

require_commands
ensure_wallet

if [[ "$PHASE" == "test" ]]; then
  echo "==> (1/3) Syncing program IDs (using prebuilt artifacts)"
  sync_program_ids
else
  echo "==> (1/3) Syncing program IDs and building"
  sync_program_ids
  build_programs_dev
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

echo "==> (2/3) Preparing validator genesis"
fetch_devnet_clones

# Spec files that start on a cluster running a program's previous build,
# which they upgrade in place: spec file -> program name.
declare -A PREVIOUS_BUILD_SPECS=([tests/forwarder-upgrade.ts]=spl_token_forwarder)

trap 'stop_validator' EXIT

echo "==> (3/3) Running ${#SPEC_FILES[@]} spec file(s), each on a fresh validator"
for i in "${!SPEC_FILES[@]}"; do
  spec="${SPEC_FILES[$i]}"
  echo "==> [$((i + 1))/${#SPEC_FILES[@]}] ${spec}"
  workspace_program_args "${PREVIOUS_BUILD_SPECS[$spec]:-}"
  start_validator "${WORKSPACE_PROGRAM_ARGS[@]}"
  if ! ANCHOR_PROVIDER_URL="$CLUSTER_URL" \
    ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
    yarn run ts-mocha --type-check -p ./tsconfig.json -t 1000000 "$spec"; then
    echo "❌ ${spec} failed (validator log: ${VALIDATOR_LOG})" >&2
    exit 1
  fi
  stop_validator
done

if [[ "$PHASE" != "test" ]]; then
  # Anchor's SBF toolchain can leave incompatible host debug artifacts in
  # target/. Clean host packages so subsequent nix cargo commands always
  # rebuild with nix rustc. (The 'test' phase compiles nothing.)
  clean_args=()
  for name in "${PROGRAM_NAMES[@]}"; do
    clean_args+=(--package "${PROGRAM_PACKAGE[$name]}")
  done
  cargo clean "${clean_args[@]}"
fi

echo "==> All tests passed"
