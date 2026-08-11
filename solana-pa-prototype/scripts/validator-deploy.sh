#!/usr/bin/env bash
# Shared build, program-ID-sync, and validator lifecycle functions.
# Source this file from other scripts; do not execute directly.
# Consumers: anchor-test.sh (local integration flow) and ops.sh (cluster ops).
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
#   PA_ID, BTF_ID, TF_ID
#
# Exported after start_validator:
#   VALIDATOR_PID

: "${PROJECT_DIR:=$(pwd)}"

CLUSTER_URL="${CLUSTER_URL:-http://127.0.0.1:8899}"
VALIDATOR_LEDGER="${VALIDATOR_LEDGER:-${PROJECT_DIR}/.validator-ledger}"
VALIDATOR_LOG="${VALIDATOR_LOG:-${PROJECT_DIR}/.validator.log}"
ANCHOR_WALLET_PATH="${ANCHOR_WALLET:-$HOME/.config/solana/id.json}"

# RISC0 verifier programs and PDAs cloned from devnet
VERIFIER_ROUTER="BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg"
GROTH16_VERIFIER="2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD"
ROUTER_PDA="9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S"
VERIFIER_ENTRY_PDA="4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey"
# Selector registered for the groth16 verifier entry above
GROTH16_SELECTOR="0x73c457ba"

VALIDATOR_PID=""

# Package names the build functions below build by name. A program added
# under programs/ that isn't in this list would otherwise silently stop being
# built on a forward-merge — fail loudly instead. This is the single copy:
# dev.sh and ops.sh both route builds through the functions in this file.
EXPECTED_PROGRAMS=(protocol-adapter block-time-forwarder test-forwarder)

assert_known_programs() {
  local dir pkg known ok
  for dir in "$PROJECT_DIR"/programs/*/; do
    pkg="$(sed -n 's/^name = "\(.*\)"$/\1/p' "${dir}Cargo.toml" | head -1)"
    ok=0
    for known in "${EXPECTED_PROGRAMS[@]}"; do
      if [[ "$pkg" == "$known" ]]; then
        ok=1
        break
      fi
    done
    if [[ $ok -eq 0 ]]; then
      echo "    ❌ Unrecognized program under programs/: '${pkg}' (${dir})" >&2
      echo "       Builds and deploys go by name. Add '${pkg}' to EXPECTED_PROGRAMS" >&2
      echo "       and the build functions in validator-deploy.sh — and to the" >&2
      echo "       PROGRAMS registry in ops.sh if it deploys to real clusters —" >&2
      echo "       otherwise it silently never gets built or deployed." >&2
      exit 1
    fi
  done
}

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

# ── Sync helpers ───────────────────────────────────────────────────────

sync_program_id() {
  local name="$1"
  local keypair="$2"
  local lib_rs="$3"
  local anchor_key="$4"

  local id
  id="$(solana-keygen pubkey "$keypair")"

  # Safety: verify the keypair matches what's committed in git.
  # If someone accidentally regenerated a keypair, this catches it
  # before we silently rewrite declare_id! to a new program ID.
  local committed_keypair
  committed_keypair="$(git show HEAD:"$keypair" 2>/dev/null || true)"
  if [[ -n "$committed_keypair" ]]; then
    local committed_id
    committed_id="$(echo "$committed_keypair" | solana-keygen pubkey /dev/stdin 2>/dev/null || true)"
    if [[ -n "$committed_id" && "$committed_id" != "$id" ]]; then
      echo "    ❌ ${name} keypair was regenerated! Local: $id, committed: $committed_id" >&2
      echo "    Restore with: git checkout HEAD -- $keypair" >&2
      exit 1
    fi
  fi

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

PROGRAM_KEYPAIRS=(
  target/deploy/protocol_adapter-keypair.json
  target/deploy/block_time_forwarder-keypair.json
  target/deploy/test_forwarder-keypair.json
)

# Restore committed program keypairs from git if missing locally.
# Returns nonzero if any keypair is still missing afterward (not in git).
restore_program_keypairs() {
  local kp missing=false
  for kp in "${PROGRAM_KEYPAIRS[@]}"; do
    if [[ ! -f "$kp" ]]; then
      missing=true
      break
    fi
  done
  if [[ "$missing" == "true" ]]; then
    # git show/checkout use paths relative to repo root, not working dir
    local git_root
    git_root="$(git rev-parse --show-prefix 2>/dev/null)"
    if git show "HEAD:${git_root}target/deploy/protocol_adapter-keypair.json" >/dev/null 2>&1; then
      echo "    Restoring program keypairs from git..."
      git checkout HEAD -- target/deploy/
    fi
  fi
  for kp in "${PROGRAM_KEYPAIRS[@]}"; do
    [[ -f "$kp" ]] || return 1
  done
}

# Keypairs are committed to the repo. If missing locally, restore from git.
# If not in git (fresh repo / CI), generate them via anchor build.
ensure_program_keypairs() {
  if ! restore_program_keypairs; then
    echo "    Generating program keypairs (first build)..."
    build_programs_dev noidl
  fi
}

# yarn install, regenerating the lockfile if it is out of sync.
ensure_node_modules() {
  if ! yarn install --frozen-lockfile; then
    echo "    Lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi
}

# Packages whose commit hash must match across every Cargo.lock in the project.
# Each entry is a workspace dep that appears in all three lockfiles (workspace,
# fixture-gen, guest). Skew silently produces proofs that don't verify on chain
# (arm-risc0) or compile errors that look like unrelated bugs (anoma-pa-solana-client).
LOCK_SYNC_PACKAGES=(anoma-rm-core anoma-pa-solana-client)

# Every Cargo.lock that participates in the build. New independent workspaces
# (a fresh tools/* crate, a separate guest, etc.) must be added here AND the
# consumers must keep working when one of these lockfiles is absent on a branch.
LOCK_FILES=(
  Cargo.lock
  tools/fixture-gen/Cargo.lock
  tools/fixture-gen/passthrough-logic/methods/guest/Cargo.lock
)

# Print the locked commit of <pkg> in <lockfile>, or empty string if absent.
lock_commit_for() {
  local lockfile="$1"
  local pkg="$2"
  if [[ ! -f "$lockfile" ]]; then
    return 0
  fi
  # `grep` returns 1 when this lockfile doesn't include $pkg — a legitimate
  # case (different lockfiles have different dep sets, e.g. the guest lockfile
  # doesn't include workspace-only deps like anoma-pa-solana-client). Suppress
  # so callers under `set -e` see the empty string instead of an early exit.
  grep -A2 "^name = \"${pkg}\"$" "$lockfile" \
    | grep "^source" \
    | grep -oP '#\K[a-f0-9]+' \
    | head -1 \
    || true
}

# Verify every package in LOCK_SYNC_PACKAGES resolves to the same commit
# across every present lockfile. Exits non-zero if any skew is detected.
ensure_lockfile_sync() {
  local pkg lockfile commit ref_commit ref_file mismatch=0
  for pkg in "${LOCK_SYNC_PACKAGES[@]}"; do
    ref_commit=''
    ref_file=''
    for lockfile in "${LOCK_FILES[@]}"; do
      local full="$PROJECT_DIR/$lockfile"
      [[ -f "$full" ]] || continue
      commit="$(lock_commit_for "$full" "$pkg")"
      [[ -n "$commit" ]] || continue
      if [[ -z "$ref_commit" ]]; then
        ref_commit="$commit"
        ref_file="$lockfile"
      elif [[ "$commit" != "$ref_commit" ]]; then
        if [[ $mismatch -eq 0 ]]; then
          echo "❌ Cargo.lock skew detected:" >&2
        fi
        echo "  ${pkg}:" >&2
        echo "    ${ref_file}: ${ref_commit:0:12}" >&2
        echo "    ${lockfile}: ${commit:0:12}" >&2
        mismatch=1
        ref_commit="$commit"
        ref_file="$lockfile"
      fi
    done
  done
  if [[ $mismatch -ne 0 ]]; then
    echo "" >&2
    echo "Fix: ./scripts/dev.sh lock-sync <package>" >&2
    return 1
  fi
}

sync_program_ids() {
  ensure_node_modules
  ensure_program_keypairs

  PA_OLD="$(read_declare_id "programs/solana-pa-prototype/src/lib.rs")"
  PA_ID="$(sync_program_id "PA" \
    "target/deploy/protocol_adapter-keypair.json" \
    "programs/solana-pa-prototype/src/lib.rs" \
    "protocol_adapter")"

  # The preloaded root-marker account fixtures derive their addresses and
  # owner from the PA program ID — regenerate them on rotation, or the
  # imported-transfer test fails NonExistingRoot against stale addresses.
  if [[ "$PA_OLD" != "$PA_ID" ]]; then
    echo "    PA ID changed — regenerating root-marker account fixtures" >&2
    npx ts-node -P tsconfig.json scripts/regen-root-markers.ts >&2
  fi

  BTF_OLD="$(read_declare_id "programs/block-time-forwarder/src/lib.rs")"
  BTF_ID="$(sync_program_id "BTF" \
    "target/deploy/block_time_forwarder-keypair.json" \
    "programs/block-time-forwarder/src/lib.rs" \
    "block_time_forwarder")"

  # BTF ID also appears in fixture-gen and integration tests
  if [[ "$BTF_OLD" != "$BTF_ID" ]]; then
    sed -i -E "s/decode_base58_32\(\"${BTF_OLD}\"\)/decode_base58_32(\"${BTF_ID}\")/" tools/fixture-gen/src/main.rs
    sed -i -E "s/blockTimeForwarderId = new PublicKey\(\"[^\"]+\"\)/blockTimeForwarderId = new PublicKey(\"${BTF_ID}\")/" tests/solana-pa-prototype.ts
  fi

  TF_OLD="$(read_declare_id "programs/test-forwarder/src/lib.rs")"
  TF_ID="$(sync_program_id "TF" \
    "target/deploy/test_forwarder-keypair.json" \
    "programs/test-forwarder/src/lib.rs" \
    "test_forwarder")"

  # TF ID also appears in fixture-gen and integration tests
  if [[ "$TF_OLD" != "$TF_ID" ]]; then
    sed -i -E "s/decode_base58_32\(\"${TF_OLD}\"\)/decode_base58_32(\"${TF_ID}\")/" tools/fixture-gen/src/main.rs
    sed -i -E "s/testForwarderId = new PublicKey\(\"[^\"]+\"\)/testForwarderId = new PublicKey(\"${TF_ID}\")/" tests/solana-pa-prototype.ts
  fi

}

# anchor build uses cargo +nightly for IDL generation, which is incompatible
# with debug artifacts compiled by the stable toolchain (e.g. from cargo test).
# Remove incremental build state and proc-macro artifacts to avoid ABI mismatch.
clean_incremental_artifacts() {
  rm -rf target/debug/incremental target/debug/build
}

build_programs_dev() {
  # $1 = "noidl" skips IDL generation — a separate cargo +nightly compile
  # pass per program that pure compile checks don't need. Anything that runs
  # the TS operator scripts or tests needs the IDL (and target/types).
  local idl_flag=""
  if [[ "${1:-}" == "noidl" ]]; then
    idl_flag="--no-idl"
  fi

  assert_known_programs
  clean_incremental_artifacts

  echo "    Building programs (development build, dev-teardown enabled)..."
  # dev-teardown enables close_markers_batch (development-only marker PDA
  # reclamation). It's a protocol-adapter-only Cargo feature, so it must
  # be scoped with -p rather than passed to the whole-workspace build.
  build_with_filtered_output anchor build -p protocol-adapter ${idl_flag} -- --features dev-teardown
  build_with_filtered_output anchor build -p block-time-forwarder ${idl_flag}
  build_with_filtered_output anchor build -p test-forwarder ${idl_flag}
}

# The production build: plain `anchor build`, no dev-teardown feature, so
# close_markers_batch must be absent from the deployed binary. The IDL is
# generated for protocol-adapter and checked, so this stays a
# self-checking command rather than a convention nothing enforces.
build_programs_release() {
  assert_known_programs
  clean_incremental_artifacts

  local idl_path="target/idl/protocol_adapter.json"
  rm -f "$idl_path"

  echo "    Building programs (production build)..."
  build_with_filtered_output anchor build -p protocol-adapter
  build_with_filtered_output anchor build -p block-time-forwarder --no-idl
  build_with_filtered_output anchor build -p test-forwarder --no-idl

  if [[ ! -f "$idl_path" ]]; then
    echo "❌ release build: anchor build did not produce an IDL at ${idl_path}" >&2
    exit 1
  fi
  if grep -q '"close_markers_batch"' "$idl_path"; then
    echo "❌ release build: close_markers_batch is present in the production IDL (${idl_path})." >&2
    echo "   dev-teardown must not be enabled for a production build." >&2
    exit 1
  fi
  echo "    ✅ Production build: close_markers_batch is absent from the IDL."
}

# Validate the required fixture contains the current BTF program ID.
# Requires BTF_ID (exported by sync_program_ids).
check_required_fixture() {
  local required_fixture="tests/fixtures/batch_groth16.json"
  if [[ ! -f "$required_fixture" ]] || ! fixture_matches_program_id "$required_fixture" "$BTF_ID"; then
    echo "Required fixture is missing or stale: ${required_fixture}"
    echo "Regenerate with: ./scripts/dev.sh gen-fixtures tests/fixtures/batch_groth16.json"
    exit 1
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

  local account_args=()
  local marker_glob="tests/fixtures/anomapay-root-markers/root-marker-"*.json
  for marker_file in $marker_glob; do
    [[ -e "$marker_file" ]] || continue
    local marker_base marker_addr
    marker_base="$(basename "$marker_file")"
    marker_addr="${marker_base#root-marker-}"
    marker_addr="${marker_addr%.json}"
    account_args+=(--account "$marker_addr" "$marker_file")
  done

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
    "${account_args[@]}" \
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
  anchor deploy --provider.cluster "$CLUSTER_URL" --program-name protocol_adapter
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
