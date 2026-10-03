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
# validator's genesis. The copies are committed in DEVNET_CLONE_DIR, so tests
# never touch the network; refresh_devnet_verifier replaces them.
VERIFIER_ROUTER="BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg"
GROTH16_VERIFIER="2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD"
ROUTER_PDA="9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S"
VERIFIER_ENTRY_PDA="4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey"
DEVNET_CLONE_PROGRAMS=("$VERIFIER_ROUTER" "$GROTH16_VERIFIER")
DEVNET_CLONE_ACCOUNTS=("$ROUTER_PDA" "$VERIFIER_ENTRY_PDA")
DEVNET_CLONE_DIR="${PROJECT_DIR}/devnet-verifier"
# Selector registered for the groth16 verifier entry above
GROTH16_SELECTOR="0x73c457ba"
# Selector the synthetic genesis VerifierEntry registers the localnet
# mock verifier under (risc0 fake-receipt convention)
MOCK_SELECTOR="0xffffffff"

VALIDATOR_PID=""

# The workspace programs (every package under programs/), one row each,
# keyed by the program's [lib] name: its Anchor program name, its key in
# Anchor.toml, and the basename of its binary and keypair in target/deploy/.
# Every build, program-ID sync, genesis load, deploy, lint, and cleanup
# iterates this table; load_workspace_programs fails if it and programs/
# disagree. Columns:
#   target        ops.sh deploy target; "-" marks a localnet-only program
#                 (integration-suite support, never deployed to a cluster)
#   sol           SOL a cluster deploy of it needs: rent for its binary
#                 (production builds measured at PA 532K ~3.7, BTF 73K ~0.5,
#                 STF 339K ~2.4) plus fee headroom; "-" for localnet-only
#                 programs
#   dev_features  Cargo features of the development build ("-": none)
#   dev_only_ix   the instructions those features add, which the production
#                 build's IDL must lack (build_programs_release checks it)
PROGRAM_TABLE="
protocol_adapter      pa   5  dev-teardown  close_markers_batch,dev_set_schema_version
block_time_forwarder  btf  2  -             -
spl_token_forwarder   stf  3  dev-config-version  dev_set_config_version
test_forwarder        -    -  -             -
mock_verifier         -    -  -             -
"

# Parsed from PROGRAM_TABLE: PROGRAM_NAMES and PROGRAM_TARGETS in table
# order; per name, PROGRAM_DEV_FEATURES and PROGRAM_DEV_ONLY_IX; per target,
# PROGRAM_BY_TARGET (its program name) and PROGRAM_DEPLOY_SOL.
PROGRAM_NAMES=()
PROGRAM_TARGETS=()
declare -A PROGRAM_DEV_FEATURES=() PROGRAM_DEV_ONLY_IX=() PROGRAM_BY_TARGET=() PROGRAM_DEPLOY_SOL=()

parse_program_table() {
  local name target sol features dev_ix
  while read -r name target sol features dev_ix; do
    [[ -n "$name" ]] || continue
    PROGRAM_NAMES+=("$name")
    PROGRAM_DEV_FEATURES[$name]="$features"
    PROGRAM_DEV_ONLY_IX[$name]="$dev_ix"
    if [[ "$target" != "-" ]]; then
      PROGRAM_TARGETS+=("$target")
      PROGRAM_BY_TARGET[$target]="$name"
      PROGRAM_DEPLOY_SOL[$target]="$sol"
    fi
  done <<<"$PROGRAM_TABLE"
}
parse_program_table
# The deploy targets as a usage alternation, e.g. "pa|btf|stf".
DEPLOY_TARGETS="$(IFS='|'; echo "${PROGRAM_TARGETS[*]}")"

# Per program name, from cargo metadata: its Cargo package name and the path
# of its lib.rs (directory names do not track program names — the PA's is
# solana-pa-prototype). Filled by load_workspace_programs, which needs cargo,
# so sourcing this file stays safe outside the Nix shell.
declare -A PROGRAM_PACKAGE=() PROGRAM_SRC=()

load_workspace_programs() {
  if [[ ${#PROGRAM_SRC[@]} -gt 0 ]]; then
    return 0
  fi
  local metadata rows lib pkg src name
  metadata="$(cargo metadata --no-deps --format-version=1 --manifest-path "$PROJECT_DIR/Cargo.toml")"
  # Every workspace member is a program (members = programs/*).
  rows="$(node -e '
    const { packages } = JSON.parse(require("fs").readFileSync(0, "utf-8"));
    for (const p of packages) {
      const lib = p.targets.find((t) => t.kind.includes("lib"));
      if (!lib) {
        console.error(`workspace package ${p.name} has no lib target`);
        process.exit(1);
      }
      console.log(lib.name, p.name, lib.src_path);
    }
  ' <<<"$metadata")"
  while read -r lib pkg src; do
    if [[ -z "${PROGRAM_DEV_FEATURES[$lib]+set}" ]]; then
      echo "    ❌ Program '${lib}' (package ${pkg}) is not in PROGRAM_TABLE (scripts/validator-deploy.sh)." >&2
      echo "       Builds, ID sync, and deploys iterate that table; add a row for it." >&2
      exit 1
    fi
    PROGRAM_PACKAGE[$lib]="$pkg"
    PROGRAM_SRC[$lib]="$src"
  done <<<"$rows"
  for name in "${PROGRAM_NAMES[@]}"; do
    if [[ -z "${PROGRAM_SRC[$name]+set}" ]]; then
      echo "    ❌ PROGRAM_TABLE lists '${name}', which is not a program under programs/." >&2
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

# The program ID of <name>, from its deploy keypair.
get_program_id() {
  solana-keygen pubkey "target/deploy/${1}-keypair.json"
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

# The spec files that build on whatever state they find: every tests/**/*.ts
# outside tests/utils/ (the support modules), tests/fresh/ and
# tests/terminal/, sorted. A cluster run runs these.
history_spec_files() {
  find tests -name '*.ts' -not -path 'tests/utils/*' -not -path 'tests/fresh/*' -not -path 'tests/terminal/*' |
    LC_ALL=C sort
}

# Every spec file in the order the local suite runs them against one
# validator: tests/fresh/ (they only work on a fresh deployment), then
# history_spec_files, then tests/terminal/ (they change the deployment for
# good), each group sorted.
suite_spec_files() {
  find tests/fresh -name '*.ts' | LC_ALL=C sort
  history_spec_files
  find tests/terminal -name '*.ts' | LC_ALL=C sort
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

# Point <name>'s declare_id! and Anchor.toml entries at its deploy keypair.
# Requires load_workspace_programs.
sync_program_id() {
  local name="$1"
  local keypair="target/deploy/${name}-keypair.json"
  local lib_rs="${PROGRAM_SRC[$name]}"

  local id
  id="$(get_program_id "$name")"

  # Safety: verify the keypair matches what's committed in git.
  # If someone accidentally regenerated a keypair, this catches it
  # before we silently rewrite declare_id! to a new program ID.
  # A keypair not yet committed has nothing to compare against.
  # (`HEAD:./path` resolves against the working directory; a bare
  # `HEAD:path` resolves against the repository root.)
  if git cat-file -e "HEAD:./${keypair}"; then
    local committed_id
    committed_id="$(git show "HEAD:./${keypair}" | solana-keygen pubkey /dev/stdin)"
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
    sed -i -E "s/^${name} = \"[^\"]+\"$/${name} = \"${id}\"/" Anchor.toml
  else
    echo "    ${name} program ID already synced: $id" >&2
  fi
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

# Restore committed program keypairs from git if missing locally.
# Returns nonzero if any keypair is still missing afterward (not in git).
restore_program_keypairs() {
  local name missing=false
  for name in "${PROGRAM_NAMES[@]}"; do
    if [[ ! -f "target/deploy/${name}-keypair.json" ]]; then
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
  for name in "${PROGRAM_NAMES[@]}"; do
    [[ -f "target/deploy/${name}-keypair.json" ]] || return 1
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

# Packages whose locked version must match across every Cargo.lock that
# contains them: the version for a crates.io package, the commit for a git
# one. Skew silently produces proofs that don't verify on chain (arm-risc0) or
# compile errors that look like unrelated bugs (anoma-pa-solana-client, which
# the guest lockfile does not contain).
LOCK_SYNC_PACKAGES=(anoma-rm-core anoma-pa-solana-client)

# Every Cargo.lock that participates in the build. New independent workspaces
# (a fresh tools/* crate, a separate guest, etc.) must be added here AND the
# consumers must keep working when one of these lockfiles is absent on a branch.
LOCK_FILES=(
  Cargo.lock
  tools/fixture-gen/Cargo.lock
  tools/fixture-gen/passthrough-logic/methods/guest/Cargo.lock
)

# Print the locked pin of <pkg> in <lockfile>: "<version>" for a crates.io
# package, "git <commit>" for a git one; nothing if the lockfile lacks it.
lock_pin_for() {
  local lockfile="$1"
  local pkg="$2"
  if [[ ! -f "$lockfile" ]]; then
    return 0
  fi
  # A lockfile without $pkg is a legitimate case: different lockfiles have
  # different dep sets (the guest lockfile lacks workspace-only deps like
  # anoma-pa-solana-client).
  local block
  if ! block="$(grep -A2 "^name = \"${pkg}\"$" "$lockfile")"; then
    return 0
  fi
  if [[ "$block" =~ source\ =\ \"git\+[^\"#]*#([a-f0-9]+)\" ]]; then
    echo "git ${BASH_REMATCH[1]}"
  elif [[ "$block" =~ source\ =\ \"registry\+ && "$block" =~ version\ =\ \"([^\"]+)\" ]]; then
    echo "${BASH_REMATCH[1]}"
  else
    echo "❌ ${lockfile}: cannot read the locked version of ${pkg}:" >&2
    echo "$block" >&2
    return 1
  fi
}

# Verify every package in LOCK_SYNC_PACKAGES resolves to the same pin across
# every present lockfile that contains it. Exits non-zero if any skew is
# detected.
ensure_lockfile_sync() {
  local pkg lockfile commit ref_commit ref_file mismatch=0
  for pkg in "${LOCK_SYNC_PACKAGES[@]}"; do
    ref_commit=''
    ref_file=''
    for lockfile in "${LOCK_FILES[@]}"; do
      local full="$PROJECT_DIR/$lockfile"
      [[ -f "$full" ]] || continue
      commit="$(lock_pin_for "$full" "$pkg")" || return 1
      [[ -n "$commit" ]] || continue
      if [[ -z "$ref_commit" ]]; then
        ref_commit="$commit"
        ref_file="$lockfile"
      elif [[ "$commit" != "$ref_commit" ]]; then
        if [[ $mismatch -eq 0 ]]; then
          echo "❌ Cargo.lock skew detected:" >&2
        fi
        echo "  ${pkg}:" >&2
        echo "    ${ref_file}: ${ref_commit}" >&2
        echo "    ${lockfile}: ${commit}" >&2
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
  load_workspace_programs

  local name mv_before
  mv_before="$(read_declare_id "${PROGRAM_SRC[mock_verifier]}")"
  for name in "${PROGRAM_NAMES[@]}"; do
    sync_program_id "$name"
  done

  # The preloaded mock VerifierEntry genesis account embeds the mock
  # verifier's ID — regenerate it on rotation.
  if [[ "$(read_declare_id "${PROGRAM_SRC[mock_verifier]}")" != "$mv_before" ]]; then
    echo "    mock_verifier ID changed — regenerating mock verifier-entry account fixture" >&2
    npx ts-node -P tsconfig.json scripts/regen-mock-verifier-entry.ts "$(get_program_id mock_verifier)" >&2
  fi
}

# The Cargo arguments of <name>'s development build, into DEV_CARGO_ARGS.
dev_cargo_args() {
  DEV_CARGO_ARGS=()
  if [[ "${PROGRAM_DEV_FEATURES[$1]}" != "-" ]]; then
    DEV_CARGO_ARGS=(-- --features "${PROGRAM_DEV_FEATURES[$1]}")
  fi
}

build_programs_dev() {
  # $1 = "noidl" skips IDL generation — a separate host `cargo test` compile
  # pass per program that pure compile checks don't need. Anything that runs
  # the TS operator scripts or tests needs the IDLs (and target/types): they
  # resolve every program, and its ID, through the Anchor workspace.
  local idl_flag=() name
  if [[ "${1:-}" == "noidl" ]]; then
    idl_flag=(--no-idl)
  fi

  load_workspace_programs

  echo "    Building programs (development build)..."
  # One build per program: dev features are per-package Cargo features.
  for name in "${PROGRAM_NAMES[@]}"; do
    dev_cargo_args "$name"
    anchor_build -p "$name" "${idl_flag[@]}" "${DEV_CARGO_ARGS[@]}"
  done
}

# The development IDLs and TypeScript types alone, without the SBF compile:
# what the TS scripts and spec files compile against, for a run against
# programs that are already deployed. The specs reference dev-only
# instructions (in tests a cluster run skips), so the types must be the
# development build's, whichever build last wrote target/.
build_dev_idls() {
  local name
  load_workspace_programs
  echo "    Generating the development IDLs and types..."
  mkdir -p target/idl target/types
  for name in "${PROGRAM_NAMES[@]}"; do
    dev_cargo_args "$name"
    anchor idl build -p "$name" -o "target/idl/${name}.json" -t "target/types/${name}.ts" "${DEV_CARGO_ARGS[@]}"
  done
}

# The production build: plain `anchor build` of the cluster programs, none of
# their dev features. Each program with dev features gets its production IDL
# checked against its development IDL, so this stays a self-checking command
# rather than a convention nothing enforces.
build_programs_release() {
  load_workspace_programs

  local target name idl_path
  echo "    Building the cluster programs (production build)..."
  for target in "${PROGRAM_TARGETS[@]}"; do
    name="${PROGRAM_BY_TARGET[$target]}"
    idl_path="target/idl/${name}.json"
    rm -f "$idl_path"
    anchor_build -p "$name"
    if [[ ! -f "$idl_path" ]]; then
      echo "❌ release build: anchor build did not produce an IDL at ${idl_path}" >&2
      exit 1
    fi
    if [[ "${PROGRAM_DEV_FEATURES[$name]}" != "-" ]]; then
      assert_release_idl_lacks_dev_only "$name"
    fi
  done
}

# <name>'s production IDL (target/idl) must be exactly its development IDL
# minus the dev-only instructions PROGRAM_TABLE declares, and each declared
# instruction must be in the development IDL. So the check fails when a
# dev-only instruction reaches the production build, and when the development
# build gains or loses anything the declaration does not account for. The
# development IDL comes from the IDL half of the development build (same
# features), without the SBF compile.
assert_release_idl_lacks_dev_only() {
  local name="$1"
  local dev_idl
  dev_idl="$(mktemp --suffix .json)"
  dev_cargo_args "$name"
  anchor idl build -p "$name" -o "$dev_idl" "${DEV_CARGO_ARGS[@]}"
  if ! node -e '
    const fs = require("fs");
    const [name, devPath, releasePath, declaredList] = process.argv.slice(1);
    const dev = JSON.parse(fs.readFileSync(devPath, "utf-8"));
    const release = JSON.parse(fs.readFileSync(releasePath, "utf-8"));
    const declared = declaredList === "-" ? [] : declaredList.split(",");
    const devNames = dev.instructions.map((ix) => ix.name);
    const releaseNames = release.instructions.map((ix) => ix.name);
    const problems = [];
    const report = (what, names) => names.length && problems.push(`${what}: ${names.join(", ")}`);
    report("declared dev-only but absent from the development IDL", declared.filter((n) => !devNames.includes(n)));
    report("dev-only instruction present in the production IDL", declared.filter((n) => releaseNames.includes(n)));
    report(
      "only in the development IDL but not declared dev-only",
      devNames.filter((n) => !releaseNames.includes(n) && !declared.includes(n)),
    );
    report("only in the production IDL", releaseNames.filter((n) => !devNames.includes(n)));
    const expected = { ...dev, instructions: dev.instructions.filter((ix) => !declared.includes(ix.name)) };
    if (problems.length === 0 && JSON.stringify(expected) !== JSON.stringify(release)) {
      problems.push("the IDLs differ beyond the declared dev-only instructions (types, accounts, events, or instruction signatures)");
    }
    if (problems.length > 0) {
      console.error(`❌ release build: ${name} production IDL (${releasePath}) is not its development IDL minus the declared dev-only instructions (${declaredList}):`);
      problems.forEach((p) => console.error(`   - ${p}`));
      process.exit(1);
    }
    console.log(`    ✅ Production build: ${name} IDL is the development IDL minus its dev-only instructions (${declared.join(", ")}).`);
  ' "$name" "$dev_idl" "target/idl/${name}.json" "${PROGRAM_DEV_ONLY_IX[$name]}"; then
    rm -f "$dev_idl"
    exit 1
  fi
  rm -f "$dev_idl"
}

# Validate the required fixture for the given test mode ($1, real|mock):
# it must exist, contain the current BTF program ID, and carry the selector
# its mode demands — a mock fixture in the real dir (or vice versa) would
# silently run the suite against the wrong verifier.
check_required_fixture() {
  local mode="$1"
  local subdir="" expected_selector="$GROTH16_SELECTOR"
  if [[ "$mode" == "mock" ]]; then
    subdir="mock/"
    expected_selector="$MOCK_SELECTOR"
  fi
  local required_fixture="tests/fixtures/${subdir}batch_groth16.json"
  if [[ ! -f "$required_fixture" ]] ||
    ! fixture_matches_program_id "$required_fixture" "$(get_program_id block_time_forwarder)" "$expected_selector"; then
    echo "Required fixture is missing, stale, or carries the wrong selector"
    echo "for ${mode} mode (expected ${expected_selector}): ${required_fixture}"
    echo "Regenerate with: ./scripts/dev.sh regen-fixtures ${mode}"
    exit 1
  fi
}

# Replace the committed copy of the devnet RISC0 verifier stack in
# DEVNET_CLONE_DIR with devnet's current state, read through the RPC endpoint
# $1: each program's binary and its devnet upgrade authority (the groth16
# verifier's authority is the router PDA, which the router relies on), and
# each PDA's account data.
refresh_devnet_verifier() {
  local rpc="$1" id addr
  mkdir -p "$DEVNET_CLONE_DIR"
  echo "Copying the devnet verifier stack into ${DEVNET_CLONE_DIR}"
  for id in "${DEVNET_CLONE_PROGRAMS[@]}"; do
    solana program dump --url "$rpc" "$id" "${DEVNET_CLONE_DIR}/${id}.so"
    solana program show --url "$rpc" "$id" --output json |
      jq -j --arg id "$id" '.authority // error("devnet program \($id) reports no upgrade authority")' \
        >"${DEVNET_CLONE_DIR}/${id}.authority"
  done
  for addr in "${DEVNET_CLONE_ACCOUNTS[@]}"; do
    solana account --url "$rpc" "$addr" --output json --output-file "${DEVNET_CLONE_DIR}/${addr}.json" >/dev/null
  done
}

# Set WORKSPACE_PROGRAM_ARGS to the solana-test-validator arguments that load
# every workspace program at genesis from target/deploy, upgradeable, with
# the provider wallet as upgrade authority (what `anchor deploy` would set).
workspace_program_args() {
  local name so
  WORKSPACE_PROGRAM_ARGS=()
  for name in "${PROGRAM_NAMES[@]}"; do
    so="target/deploy/${name}.so"
    if [[ ! -f "$so" ]]; then
      echo "❌ ${so} is missing; build the programs first ('./scripts/anchor-test.sh build', or the default phase)." >&2
      exit 1
    fi
    WORKSPACE_PROGRAM_ARGS+=(--upgradeable-program "target/deploy/${name}-keypair.json" "$so" "$ANCHOR_WALLET_PATH")
  done
}

# Start a validator on a fresh ledger with the devnet verifier stack (the
# committed copy in DEVNET_CLONE_DIR) and the genesis account fixtures preloaded. Extra
# arguments are passed to solana-test-validator (e.g. WORKSPACE_PROGRAM_ARGS).
start_validator() {
  local clone_args=() id addr file
  for id in "${DEVNET_CLONE_PROGRAMS[@]}"; do
    for file in "${DEVNET_CLONE_DIR}/${id}.so" "${DEVNET_CLONE_DIR}/${id}.authority"; do
      if [[ ! -s "$file" ]]; then
        echo "❌ ${file} is missing; restore it from git, or run ./scripts/dev.sh refresh-devnet-verifier --url <devnet rpc>." >&2
        return 1
      fi
    done
    clone_args+=(--upgradeable-program "$id" "${DEVNET_CLONE_DIR}/${id}.so" "$(<"${DEVNET_CLONE_DIR}/${id}.authority")")
  done
  for addr in "${DEVNET_CLONE_ACCOUNTS[@]}"; do
    file="${DEVNET_CLONE_DIR}/${addr}.json"
    if [[ ! -s "$file" ]]; then
      echo "❌ ${file} is missing; restore it from git, or run ./scripts/dev.sh refresh-devnet-verifier --url <devnet rpc>." >&2
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
