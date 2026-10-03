#!/usr/bin/env bash
# Local integration flow. Every spec file runs against one validator, so
# the deployment builds up as much history as the suite makes: first the
# files that only work on a fresh deployment (tests/fresh/), then every
# other file, which builds on whatever state it finds, then the files that
# change the deployment for good (tests/terminal/, in their numbered order).
# Then each upgrade-path file (tests/upgrade/<program>.ts) on a validator of
# its own, which starts on that program's previous build.
#
#   anchor-test.sh [all|build|test] [spec file...]
#
# Phase selection, so CI can build once and fan the test phase out per mode:
#   all   (default) sync IDs, build, then the spec files
#   build           sync IDs and build the programs, nothing else
#   test            the spec files against existing artifacts
# Spec files default to every tests/**/*.ts outside tests/utils/ (the
# support modules the specs import) in that order; given ones run in the
# order given, the upgrade-path ones after the rest.
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
  mapfile -t SPEC_FILES < <(suite_spec_files; upgrade_spec_files)
fi
SUITE_SPECS=()
UPGRADE_SPECS=()
for spec in "${SPEC_FILES[@]}"; do
  if [[ ! -f "$spec" ]]; then
    echo "❌ spec file not found: ${spec} (paths are relative to ${PROJECT_DIR})" >&2
    exit 1
  fi
  if [[ "$spec" == tests/upgrade/* ]]; then
    UPGRADE_SPECS+=("$spec")
  else
    SUITE_SPECS+=("$spec")
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

# The settlement lookup table the validator starts with (see
# tests/utils/genesis-settlement-table.ts); its keys depend on the test mode.
# A validator serves a table's keys only once its root is past the slot that
# last extended the table, slot 0 for this one; the validator is warped to
# slot 1 so its root starts there, instead of about 32 slots after startup.
GENESIS_ACCOUNT_DIR="${PROJECT_DIR}/.cache/genesis-accounts"
mkdir -p "$GENESIS_ACCOUNT_DIR"
ANCHOR_PROVIDER_URL="$CLUSTER_URL" ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
  npx ts-node -P tsconfig.json tests/utils/genesis-settlement-table.ts "$GENESIS_ACCOUNT_DIR"
settlement_table_file=("$GENESIS_ACCOUNT_DIR"/settlement-table-*.json)
PA_SETTLEMENT_TABLE="$(basename "${settlement_table_file[0]}" .json)"
PA_SETTLEMENT_TABLE="${PA_SETTLEMENT_TABLE#settlement-table-}"

trap 'stop_validator' EXIT

# Run one spec file against the running validator.
run_spec() {
  local spec="$1"
  if ! ANCHOR_PROVIDER_URL="$CLUSTER_URL" \
    ANCHOR_WALLET="$ANCHOR_WALLET_PATH" \
    PA_SETTLEMENT_TABLE="$PA_SETTLEMENT_TABLE" \
    yarn run ts-mocha --type-check -p ./tsconfig.json -t 1000000 "$spec"; then
    echo "❌ ${spec} failed (validator log: ${VALIDATOR_LOG})" >&2
    exit 1
  fi
}

echo "==> (3/3) Running ${#SUITE_SPECS[@]} spec file(s) on one validator, then ${#UPGRADE_SPECS[@]} upgrade-path file(s) on validators of their own"
if [[ ${#SUITE_SPECS[@]} -gt 0 ]]; then
  workspace_program_args
  start_validator "${WORKSPACE_PROGRAM_ARGS[@]}" --warp-slot 1 --account "$PA_SETTLEMENT_TABLE" "${settlement_table_file[0]}"
  for i in "${!SUITE_SPECS[@]}"; do
    echo "==> [$((i + 1))/${#SUITE_SPECS[@]}] ${SUITE_SPECS[$i]}"
    run_spec "${SUITE_SPECS[$i]}"
  done
  stop_validator
fi
for spec in "${UPGRADE_SPECS[@]}"; do
  echo "==> [upgrade path] ${spec}, starting on $(basename "$spec" .ts)'s previous build"
  workspace_program_args "$(basename "$spec" .ts)"
  start_validator "${WORKSPACE_PROGRAM_ARGS[@]}" --warp-slot 1 --account "$PA_SETTLEMENT_TABLE" "${settlement_table_file[0]}"
  run_spec "$spec"
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
