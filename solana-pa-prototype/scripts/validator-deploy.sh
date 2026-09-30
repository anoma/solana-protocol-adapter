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
#   PA_ID, BTF_ID, TF_ID, MV_ID
#
# Exported after workspace_program_args:
#   WORKSPACE_PROGRAM_ARGS
#
# Exported after start_validator:
#   VALIDATOR_PID

: "${PROJECT_DIR:=$(pwd)}"

CLUSTER_URL="${CLUSTER_URL:-http://127.0.0.1:8899}"
VALIDATOR_LEDGER="${VALIDATOR_LEDGER:-${PROJECT_DIR}/.validator-ledger}"
VALIDATOR_LOG="${VALIDATOR_LOG:-${PROJECT_DIR}/.validator.log}"
ANCHOR_WALLET_PATH="${ANCHOR_WALLET:-$HOME/.config/solana/id.json}"

# RISC0 verifier programs and PDAs copied from devnet into every local
# validator's genesis. fetch_devnet_clones downloads them once into
# DEVNET_CLONE_DIR (gitignored), so each validator start is offline.
VERIFIER_ROUTER="BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg"
GROTH16_VERIFIER="2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD"
ROUTER_PDA="9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S"
VERIFIER_ENTRY_PDA="4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey"
DEVNET_CLONE_PROGRAMS=("$VERIFIER_ROUTER" "$GROTH16_VERIFIER")
DEVNET_CLONE_ACCOUNTS=("$ROUTER_PDA" "$VERIFIER_ENTRY_PDA")
DEVNET_CLONE_DIR="${PROJECT_DIR}/.cache/devnet-clones"
# Selector registered for the groth16 verifier entry above
GROTH16_SELECTOR="0x73c457ba"
# Selector the synthetic genesis VerifierEntry registers the localnet
# mock verifier under (risc0 fake-receipt convention)
MOCK_SELECTOR="0xffffffff"

VALIDATOR_PID=""

# Package names the build functions below build by name. A program added
# under programs/ that isn't in this list would otherwise silently stop being
# built on a forward-merge — fail loudly instead. This is the single copy:
# dev.sh and ops.sh both route builds through the functions in this file.
EXPECTED_PROGRAMS=(protocol-adapter block-time-forwarder spl-token-forwarder test-forwarder mock-verifier)

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
      echo "       Builds and deploys go by name. Register '${pkg}' in ALL of:" >&2
      echo "       EXPECTED_PROGRAMS, PROGRAM_KEYPAIRS (which also drives the" >&2
      echo "       local validator's genesis), sync_program_ids, build_programs_dev," >&2
      echo "       and build_programs_release (this file), plus the PROGRAMS/PROGRAM_LIBRS" >&2
      echo "       registries in ops.sh if it deploys to real clusters —" >&2
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

# The SBPF version every program build targets: local and CI builds here, and
# the solana-verify deterministic build (ops.sh verify-build), whose
# `cargo build-sbf` would otherwise default to v0.
SBPF_ARCH="v3"

anchor_build() {
  checked_sbf_build anchor build --arch "$SBPF_ARCH" "$@"
}

# Run an SBF build command and fail if it failed or reported a stack-frame
# overflow. The SBF backend reports a function whose frame exceeds the 4 KiB
# limit as an "Error: Function ... overflows the maximum allowed frame space"
# line yet still exits 0; such a function corrupts memory when it runs on
# chain.
checked_sbf_build() {
  local build_log build_status=0 grep_status=0
  build_log="$(mktemp)"
  "$@" 2>&1 | tee "$build_log" || build_status=$?
  grep -E "overflows the maximum allowed frame space|Stack offset of .* exceeded max offset" "$build_log" || grep_status=$?
  rm "$build_log"
  if ((build_status != 0)); then
    echo "❌ $* exited with status ${build_status}." >&2
    exit "$build_status"
  fi
  case "$grep_status" in
    0)
      echo "❌ The SBF build reported stack-frame overflows (above); a program would crash on chain." >&2
      exit 1
      ;;
    1) ;; # grep found no overflow report
    *)
      echo "❌ Could not scan the build log for stack-frame overflows (grep status ${grep_status})." >&2
      exit 1
      ;;
  esac
}

fixture_matches_program_id() {
  local fixture_path="$1"
  local program_id="$2"
  local expected_selector="${3:-}"

  node -e '
    const fs = require("fs");
    const bs58 = require("bs58").default || require("bs58");

    const fixturePath = process.argv[1];
    const programId = process.argv[2];
    const expectedSelector = process.argv[3];

    const fixture = JSON.parse(fs.readFileSync(fixturePath, "utf-8"));
    if (!fixture.selector || !fixture.tx_b64) {
      process.exit(1);
    }
    if (expectedSelector && fixture.selector !== expectedSelector) {
      process.exit(1);
    }

    const txBytes = Buffer.from(fixture.tx_b64, "base64");
    const programIdBytes = Buffer.from(bs58.decode(programId));
    process.exit(txBytes.includes(programIdBytes) ? 0 : 1);
  ' "$fixture_path" "$program_id" "$expected_selector"
}

# The two suite proof modes; each entry point validates its own input.
validate_test_mode() {
  if [[ "$1" != "real" && "$1" != "mock" ]]; then
    echo "❌ test mode must be 'real' or 'mock', got '$1'" >&2
    exit 1
  fi
}

# Poll the validator's health endpoint until it answers. A refused
# connection is the expected state while it boots, so the probe's own
# output is discarded; a validator that exits during boot fails at once.
wait_for_validator() {
  local url="$1"
  local attempts=60

  for _ in $(seq 1 "$attempts"); do
    if curl -fs -o /dev/null "${url}/health"; then
      return 0
    fi
    if ! kill -0 "$VALIDATOR_PID"; then
      echo "Validator process ${VALIDATOR_PID} exited during startup" >&2
      return 1
    fi
    sleep 1
  done

  echo "Validator did not become healthy within ${attempts}s" >&2
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
  # A keypair not yet committed has nothing to compare against.
  if git cat-file -e HEAD:"$keypair"; then
    local committed_id
    committed_id="$(git show HEAD:"$keypair" | solana-keygen pubkey /dev/stdin)"
    if [[ "$committed_id" != "$id" ]]; then
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
  require_cmd jq
  require_cmd yarn
  require_cmd curl
}

PROGRAM_KEYPAIRS=(
  target/deploy/protocol_adapter-keypair.json
  target/deploy/block_time_forwarder-keypair.json
  target/deploy/spl_token_forwarder-keypair.json
  target/deploy/test_forwarder-keypair.json
  target/deploy/mock_verifier-keypair.json
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
    git_root="$(git rev-parse --show-prefix)"
    if git cat-file -e "HEAD:${git_root}target/deploy/protocol_adapter-keypair.json"; then
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
  # A lockfile without $pkg is a legitimate case: different lockfiles have
  # different dep sets (the guest lockfile lacks workspace-only deps like
  # anoma-pa-solana-client). It, and a package with no git source, print
  # nothing.
  local block
  if ! block="$(grep -A2 "^name = \"${pkg}\"$" "$lockfile")"; then
    return 0
  fi
  if [[ "$block" =~ source\ =\ \"git\+[^\"#]*#([a-f0-9]+)\" ]]; then
    echo "${BASH_REMATCH[1]}"
  fi
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

  PA_ID="$(sync_program_id "PA" \
    "target/deploy/protocol_adapter-keypair.json" \
    "programs/solana-pa-prototype/src/lib.rs" \
    "protocol_adapter")"

  BTF_OLD="$(read_declare_id "programs/block-time-forwarder/src/lib.rs")"
  BTF_ID="$(sync_program_id "BTF" \
    "target/deploy/block_time_forwarder-keypair.json" \
    "programs/block-time-forwarder/src/lib.rs" \
    "block_time_forwarder")"

  # BTF ID also appears in the integration tests (fixture-gen reads the crate's ID)
  if [[ "$BTF_OLD" != "$BTF_ID" ]]; then
    sed -i -E "s/blockTimeForwarderId = new PublicKey\(\"[^\"]+\"\)/blockTimeForwarderId = new PublicKey(\"${BTF_ID}\")/" tests/utils/adapterSuite.ts
  fi

  # Nothing else carries the STF id: fixture-gen and the tests read the crate's ID.
  sync_program_id "STF" \
    "target/deploy/spl_token_forwarder-keypair.json" \
    "programs/spl-token-forwarder/src/lib.rs" \
    "spl_token_forwarder" >/dev/null

  TF_OLD="$(read_declare_id "programs/test-forwarder/src/lib.rs")"
  TF_ID="$(sync_program_id "TF" \
    "target/deploy/test_forwarder-keypair.json" \
    "programs/test-forwarder/src/lib.rs" \
    "test_forwarder")"

  # TF ID also appears in the integration tests (fixture-gen reads the crate's ID)
  if [[ "$TF_OLD" != "$TF_ID" ]]; then
    sed -i -E "s/testForwarderId = new PublicKey\(\"[^\"]+\"\)/testForwarderId = new PublicKey(\"${TF_ID}\")/" tests/utils/adapterSuite.ts
  fi

  MV_OLD="$(read_declare_id "programs/mock-verifier/src/lib.rs")"
  MV_ID="$(sync_program_id "MV" \
    "target/deploy/mock_verifier-keypair.json" \
    "programs/mock-verifier/src/lib.rs" \
    "mock_verifier")"

  # MV ID also appears in the TS verifier utils, and the preloaded mock
  # VerifierEntry account fixture embeds it — regenerate on rotation.
  if [[ "$MV_OLD" != "$MV_ID" ]]; then
    sed -i -E "s/MOCK_VERIFIER_ID = new PublicKey\(\"[^\"]+\"\)/MOCK_VERIFIER_ID = new PublicKey(\"${MV_ID}\")/" scripts/verifier-utils/index.ts
    echo "    MV ID changed — regenerating mock verifier-entry account fixture" >&2
    npx ts-node -P tsconfig.json scripts/regen-mock-verifier-entry.ts >&2
  fi
}

build_programs_dev() {
  # $1 = "noidl" skips IDL generation — a separate host `cargo test` compile
  # pass per program that pure compile checks don't need. Anything that runs
  # the TS operator scripts or tests needs the IDL (and target/types).
  local idl_flag=""
  if [[ "${1:-}" == "noidl" ]]; then
    idl_flag="--no-idl"
  fi

  assert_known_programs

  echo "    Building programs (development build, dev-teardown enabled)..."
  # dev-teardown enables close_markers_batch (development-only marker PDA
  # reclamation). It's a protocol-adapter-only Cargo feature, so it must
  # be scoped with -p rather than passed to the whole-workspace build.
  anchor_build -p protocol_adapter ${idl_flag} -- --features dev-teardown
  anchor_build -p block_time_forwarder ${idl_flag}
  anchor_build -p spl_token_forwarder ${idl_flag}
  # Nothing consumes the test-only programs' IDLs; always skip their IDL pass.
  anchor_build -p test_forwarder --no-idl
  anchor_build -p mock_verifier --no-idl
}

# Prints the names of instructions gated by a `#[cfg(...)]` attribute whose
# argument mentions `dev-teardown` (a bare `feature = "dev-teardown"`, no
# spaces, or wrapped in `all(...)`/`any(...)`), one per line: any `pub fn`
# following such an attribute, skipping over doc comments (`///`), line
# comments (`//`), further attributes (e.g. `#[derive(Accounts)]`), and
# blank lines in between. A cfg attribute whose next item is not a `pub fn`
# (a struct, an impl block, etc.) gates that item instead of an instruction
# and is classified silently rather than printed. Every cfg occurrence found
# must land in one of those two buckets — if end-of-file arrives while an
# attribute is still waiting for its item, that occurrence is left
# unclassified and the mismatch is caught below.
dev_only_instructions() {
  awk -v src="$1" '
    /#\[cfg\(/ && /dev-teardown/ {
      cfg_count++
      pending = 1
      next
    }
    pending && /^[[:space:]]*($|#\[|\/\/)/ { next }
    pending && /pub fn/ {
      match($0, /pub fn [A-Za-z0-9_]+/)
      print substr($0, RSTART + 7, RLENGTH - 7)
      classified++
      pending = 0
      next
    }
    pending {
      classified++
      pending = 0
    }
    END {
      if (cfg_count + 0 != classified + 0) {
        printf "❌ release build: %d dev-teardown cfg attribute(s) in %s could not be classified as an instruction or an item\n", cfg_count - classified, src > "/dev/stderr"
        exit 1
      }
    }
  ' "$1"
}

# The production build: plain `anchor build`, no dev-teardown feature, so
# the dev-only instructions (derived from `#[cfg(feature = "dev-teardown")]`
# in lib.rs) must be absent from the deployed binary. The IDL is generated
# for protocol-adapter and checked, so this stays a self-checking command
# rather than a convention nothing enforces.
build_programs_release() {
  assert_known_programs

  local idl_path="target/idl/protocol_adapter.json"
  rm -f "$idl_path"

  echo "    Building programs (production build)..."
  anchor_build -p protocol_adapter
  anchor_build -p block_time_forwarder --no-idl
  # The forwarder's operator script resolves the program through the Anchor
  # workspace, which needs its IDL and types.
  anchor_build -p spl_token_forwarder
  anchor_build -p test_forwarder --no-idl
  anchor_build -p mock_verifier --no-idl

  if [[ ! -f "$idl_path" ]]; then
    echo "❌ release build: anchor build did not produce an IDL at ${idl_path}" >&2
    exit 1
  fi
  local lib_rs="$PROJECT_DIR/programs/solana-pa-prototype/src/lib.rs"
  local ix_names
  ix_names="$(dev_only_instructions "$lib_rs")"
  if [[ -z "$ix_names" ]]; then
    echo "❌ release build: no dev-teardown-gated instructions found in lib.rs; the IDL self-check cannot run" >&2
    exit 1
  fi

  local dev_only_ix
  for dev_only_ix in $ix_names; do
    if grep -q "\"${dev_only_ix}\"" "$idl_path"; then
      echo "❌ release build: ${dev_only_ix} is present in the production IDL (${idl_path})." >&2
      echo "   dev-teardown must not be enabled for a production build." >&2
      exit 1
    fi
  done
  echo "    ✅ Production build: dev-only instructions ($(echo "$ix_names" | paste -sd, -)) are absent from the IDL."
}

# Validate the required fixture for the given test mode ($1, real|mock):
# it must exist, contain the current BTF program ID, and carry the selector
# its mode demands — a mock fixture in the real dir (or vice versa) would
# silently run the suite against the wrong verifier.
# Requires BTF_ID (exported by sync_program_ids).
check_required_fixture() {
  local mode="$1"
  local subdir="" mock_flag="" expected_selector="$GROTH16_SELECTOR"
  if [[ "$mode" == "mock" ]]; then
    subdir="mock/"
    mock_flag="--mock "
    expected_selector="$MOCK_SELECTOR"
  fi
  local required_fixture="tests/fixtures/${subdir}batch_groth16.json"
  if [[ ! -f "$required_fixture" ]] ||
    ! fixture_matches_program_id "$required_fixture" "$BTF_ID" "$expected_selector"; then
    echo "Required fixture is missing, stale, or carries the wrong selector"
    echo "for ${mode} mode (expected ${expected_selector}): ${required_fixture}"
    echo "Regenerate with: ./scripts/dev.sh gen-fixtures ${mock_flag}${required_fixture}"
    exit 1
  fi
}

# Download the devnet RISC0 verifier stack into DEVNET_CLONE_DIR: each
# program's binary and its devnet upgrade authority (the groth16 verifier's
# authority is the router PDA, which the router relies on), and each PDA's
# account data. Overwrites the previous download, so every run starts from
# devnet's current state.
fetch_devnet_clones() {
  local id addr
  mkdir -p "$DEVNET_CLONE_DIR"
  echo "    Fetching the devnet verifier stack into ${DEVNET_CLONE_DIR}"
  for id in "${DEVNET_CLONE_PROGRAMS[@]}"; do
    solana program dump --url devnet "$id" "${DEVNET_CLONE_DIR}/${id}.so"
    solana program show --url devnet "$id" --output json |
      jq -j --arg id "$id" '.authority // error("devnet program \($id) reports no upgrade authority")' \
        >"${DEVNET_CLONE_DIR}/${id}.authority"
  done
  for addr in "${DEVNET_CLONE_ACCOUNTS[@]}"; do
    solana account --url devnet "$addr" --output json --output-file "${DEVNET_CLONE_DIR}/${addr}.json" >/dev/null
  done
}

# Set WORKSPACE_PROGRAM_ARGS to the solana-test-validator arguments that load
# every workspace program at genesis from target/deploy, upgradeable, with
# the provider wallet as upgrade authority (what `anchor deploy` would set).
workspace_program_args() {
  local kp so
  WORKSPACE_PROGRAM_ARGS=()
  for kp in "${PROGRAM_KEYPAIRS[@]}"; do
    so="target/deploy/$(basename "$kp" -keypair.json).so"
    if [[ ! -f "$so" ]]; then
      echo "❌ ${so} is missing; build the programs first ('./scripts/anchor-test.sh build', or the default phase)." >&2
      exit 1
    fi
    WORKSPACE_PROGRAM_ARGS+=(--upgradeable-program "$kp" "$so" "$ANCHOR_WALLET_PATH")
  done
}

# Start a validator on a fresh ledger with the devnet verifier stack (from
# fetch_devnet_clones) and the genesis account fixtures preloaded. Extra
# arguments are passed to solana-test-validator (e.g. WORKSPACE_PROGRAM_ARGS).
start_validator() {
  local clone_args=() id addr file
  for id in "${DEVNET_CLONE_PROGRAMS[@]}"; do
    for file in "${DEVNET_CLONE_DIR}/${id}.so" "${DEVNET_CLONE_DIR}/${id}.authority"; do
      if [[ ! -s "$file" ]]; then
        echo "❌ ${file} is missing; run fetch_devnet_clones first." >&2
        return 1
      fi
    done
    clone_args+=(--upgradeable-program "$id" "${DEVNET_CLONE_DIR}/${id}.so" "$(<"${DEVNET_CLONE_DIR}/${id}.authority")")
  done
  for addr in "${DEVNET_CLONE_ACCOUNTS[@]}"; do
    file="${DEVNET_CLONE_DIR}/${addr}.json"
    if [[ ! -s "$file" ]]; then
      echo "❌ ${file} is missing; run fetch_devnet_clones first." >&2
      return 1
    fi
    clone_args+=(--account "$addr" "$file")
  done

  # A process left on the RPC port (e.g. by an interrupted run) would make
  # the new validator fail to bind. lsof exits 1 when nothing listens.
  local existing_pids
  if existing_pids="$(lsof -ti :8899)"; then
    echo "Killing existing process on port 8899 (pid(s) ${existing_pids//$'\n'/ })"
    # shellcheck disable=SC2086
    kill -9 $existing_pids
    sleep 1
  fi

  mkdir -p "$VALIDATOR_LEDGER"

  # Genesis account fixtures named <prefix><address>.json, preloaded via
  # --account: AnomaPay root markers, and the synthetic VerifierEntry that
  # registers the mock verifier under selector 0xffffffff (loaded in both
  # modes — a PA instance only accepts seals with the selector it was
  # initialized with, so real-mode runs are unaffected).
  local account_args=()
  add_genesis_accounts() {
    local dir="$1" prefix="$2" file base addr
    for file in "$dir/$prefix"*.json; do
      [[ -e "$file" ]] || continue
      base="$(basename "$file")"
      addr="${base#"$prefix"}"
      addr="${addr%.json}"
      account_args+=(--account "$addr" "$file")
    done
  }
  add_genesis_accounts tests/fixtures/verifier-entries verifier-entry-

  solana-test-validator \
    --reset \
    --ledger "$VALIDATOR_LEDGER" \
    --rpc-port 8899 \
    --faucet-port 9900 \
    --bind-address 127.0.0.1 \
    "${clone_args[@]}" \
    "${account_args[@]}" \
    "$@" \
    --log \
    >"$VALIDATOR_LOG" 2>&1 &
  VALIDATOR_PID=$!

  if ! wait_for_validator "$CLUSTER_URL"; then
    echo "Validator failed to start. Last lines from ${VALIDATOR_LOG}:"
    tail -n 50 "$VALIDATOR_LOG"
    return 1
  fi
  echo "Validator ready (pid ${VALIDATOR_PID})"
}

# Stop the validator started by start_validator and reap it. It exits 143
# on the SIGTERM sent here; any other status means it had already died.
stop_validator() {
  if [[ -z "${VALIDATOR_PID:-}" ]]; then
    return 0
  fi
  local pid="$VALIDATOR_PID" status=0
  VALIDATOR_PID=""
  if ! kill "$pid"; then
    echo "Validator (pid ${pid}) had already exited" >&2
  fi
  wait "$pid" || status=$?
  if [[ $status -ne 0 && $status -ne 143 ]]; then
    echo "❌ Validator (pid ${pid}) exited with status ${status}; log: ${VALIDATOR_LOG}" >&2
    return 1
  fi
}
