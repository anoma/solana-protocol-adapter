#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_DIR="$(cd "${PROJECT_DIR}/.." && pwd)"

resolve_project_workdir() {
  if [[ "$PROJECT_DIR" != *" "* ]]; then
    printf '%s' "$PROJECT_DIR"
    return
  fi

  # Some linker/toolchain paths break on spaces. Use a stable symlink with no spaces.
  local link_root="${TMPDIR:-/tmp}/solana-pa-nix"
  local link_path="${link_root}/solana-pa-prototype"
  mkdir -p "$link_root"
  ln -sfn "$PROJECT_DIR" "$link_path"
  printf '%s' "$link_path"
}

run_in_project() {
  local cmd="$1"
  local workdir
  workdir="$(resolve_project_workdir)"
  if [[ -n "${IN_NIX_SHELL:-}" ]]; then
    (cd "$workdir" && bash --noprofile --norc -c "$cmd")
  else
    nix --extra-experimental-features 'nix-command flakes' develop "$REPO_DIR" --command bash --noprofile --norc -c "cd '$workdir' && $cmd"
  fi
}

positive_integer_or_die() {
  local name="$1"
  local value="$2"
  if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
    echo "$name must be a positive integer." >&2
    exit 1
  fi
}

fixture_build_jobs() {
  local jobs="${CARGO_BUILD_JOBS:-10}"
  positive_integer_or_die "CARGO_BUILD_JOBS" "$jobs"
  printf '%s' "$jobs"
}

fixture_test_threads() {
  local threads="${RUST_TEST_THREADS:-10}"
  positive_integer_or_die "RUST_TEST_THREADS" "$threads"
  printf '%s' "$threads"
}

fixture_worker_threads() {
  local threads="${RAYON_NUM_THREADS:-10}"
  positive_integer_or_die "RAYON_NUM_THREADS" "$threads"
  printf '%s' "$threads"
}

# Packages whose commit hash must match across every Cargo.lock in the project.
# Each entry is a workspace dep that appears in all three lockfiles (workspace,
# fixture-gen, guest). Skew silently produces proofs that don't verify on chain
# (arm-risc0) or compile errors that look like unrelated bugs (anoma-pa-solana-client).
LOCK_SYNC_PACKAGES=(anoma-rm-core anoma-pa-solana-client)

# Every Cargo.lock that participates in the build. New independent workspaces
# (a fresh tools/* crate, a separate guest, etc.) must be added here AND the
# script must keep working when one of these lockfiles is absent on a branch.
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

# Run `cargo update -p <pkg>` against every present lockfile so all three stay
# pinned to the same commit. Use after bumping a git-dep branch HEAD.
sync_lockfiles_for_package() {
  local pkg="$1"
  if [[ -z "$pkg" ]]; then
    echo "Usage: $0 lock-sync <package>" >&2
    exit 1
  fi
  local lockfile manifest_rel
  for lockfile in "${LOCK_FILES[@]}"; do
    manifest_rel="${lockfile%Cargo.lock}Cargo.toml"
    if [[ ! -f "${PROJECT_DIR}/${manifest_rel}" ]]; then
      continue
    fi
    echo "==> ${lockfile}"
    run_in_project "cargo update --manifest-path '${manifest_rel}' -p '${pkg}'"
  done
}

case "${1:-}" in
  shell)
    if [[ -n "${IN_NIX_SHELL:-}" ]]; then
      cd "$PROJECT_DIR"
      exec bash
    fi

    cd "$REPO_DIR"
    exec nix --extra-experimental-features 'nix-command flakes' develop
    ;;

  test)
    # Keep Rust test artifacts isolated from Anchor/SBF build outputs.
    run_in_project "CARGO_TARGET_DIR=target/nix-tests cargo test --workspace"
    ;;

  anchor-build)
    run_in_project "anchor build --no-idl"
    # --no-idl skips IDL generation, leaving stale addresses in target/idl/.
    # Patch them to match the current declare_id! in source.
    run_in_project '
      for lib_rs in programs/*/src/lib.rs; do
        prog_dir="$(basename "$(dirname "$(dirname "$lib_rs")")")"
        idl_name="$(echo "$prog_dir" | tr - _)"
        idl_file="target/idl/${idl_name}.json"
        if [[ -f "$idl_file" ]]; then
          source_id=$(grep -oP "declare_id!\(\"\K[^\"]+\"" "$lib_rs" | tr -d "\"")
          if [[ -n "$source_id" ]]; then
            current_id=$(python3 -c "import json; print(json.load(open(\"$idl_file\")).get(\"address\",\"\"))")
            if [[ "$current_id" != "$source_id" ]]; then
              python3 -c "
import json
with open(\"$idl_file\") as f: d = json.load(f)
d[\"address\"] = \"$source_id\"
with open(\"$idl_file\", \"w\") as f: json.dump(d, f, indent=2)
"
              echo "Patched IDL address: $idl_name ($current_id -> $source_id)"
            fi
          fi
        fi
      done
    '
    ;;

  anchor-test)
    ensure_lockfile_sync
    run_in_project "./scripts/anchor-test.sh"
    ;;

  validator)
    run_in_project "solana-test-validator --reset --url devnet --rpc-port 8899 --faucet-port 9900 --bind-address 127.0.0.1 --log"
    ;;

  gen-fixtures)
    shift
    build_jobs="$(fixture_build_jobs)"
    worker_threads="$(fixture_worker_threads)"
    echo "Fixture build jobs: $build_jobs"
    echo "Fixture worker threads: $worker_threads"
    ensure_lockfile_sync
    run_in_project "CARGO_BUILD_JOBS=$build_jobs RAYON_NUM_THREADS=$worker_threads cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- $*"
    ;;

  lock-sync)
    shift
    sync_lockfiles_for_package "${1:-}"
    ;;

  lock-check)
    ensure_lockfile_sync && echo "All Cargo.lock files are in sync."
    ;;

  fixture-test)
    build_jobs="$(fixture_build_jobs)"
    test_threads="$(fixture_test_threads)"
    worker_threads="$(fixture_worker_threads)"
    echo "Fixture build jobs: $build_jobs"
    echo "Fixture test threads: $test_threads"
    echo "Fixture worker threads: $worker_threads"
    run_in_project "RISC0_SKIP_BUILD=1 CARGO_BUILD_JOBS=$build_jobs RUST_TEST_THREADS=$test_threads RAYON_NUM_THREADS=$worker_threads cargo test --manifest-path tools/fixture-gen/Cargo.toml -- --test-threads=$test_threads"
    ;;

  update-deps)
    run_in_project "rm -f yarn.lock package-lock.json && yarn install"
    ;;

  clean)
    run_in_project "rm -rf .validator-ledger .validator.log test-ledger"
    ;;

  fmt)
    run_in_project "cargo fmt -p solana-pa-prototype -p block-time-forwarder -- --check"
    ;;

  clippy)
    run_in_project "cargo clippy -p solana-pa-prototype -p block-time-forwarder --all-targets -- -D warnings -A unexpected_cfgs -A deprecated"
    ;;

  devnet)
    shift
    run_in_project "./scripts/devnet.sh $*"
    ;;

  mainnet)
    shift
    run_in_project "./scripts/mainnet.sh $*"
    ;;

  deploy-verifier)
    shift
    run_in_project "./scripts/deploy-verifier.sh $*"
    ;;

  coverage)
    echo "Building test binaries..."
    BUILD_JSON=$(run_in_project "cargo test -p solana-pa-prototype -p block-time-forwarder --no-run --message-format=json 2>/dev/null")
    BINS=$(echo "$BUILD_JSON" | jq -r 'select(.executable != null and .profile.test == true) | .executable')

    if [[ -z "$BINS" ]]; then
      echo "❌ No test binaries found"
      exit 1
    fi

    KCOV_DIR="/tmp/kcov-solana-pa"
    rm -rf "$KCOV_DIR"
    SRC_INCLUDE="$PROJECT_DIR/programs"

    for bin in $BINS; do
      echo "Tracing: $(basename "$bin")"
      nix --extra-experimental-features 'nix-command flakes' shell nixpkgs#kcov --command \
        kcov --include-path="$SRC_INCLUDE" "$KCOV_DIR" "$bin" 2>&1 | tail -1
    done

    # Find merged coverage (or single-binary coverage)
    COV_JSON=$(find "$KCOV_DIR" -name coverage.json -path "*/kcov-merged/*" 2>/dev/null | head -1)
    if [[ -z "$COV_JSON" ]]; then
      COV_JSON=$(find "$KCOV_DIR" -name coverage.json 2>/dev/null | head -1)
    fi

    if [[ -z "$COV_JSON" ]]; then
      echo "❌ No coverage data produced"
      exit 1
    fi

    python3 << PYEOF
import json

with open("$COV_JSON") as f:
    data = json.load(f)

files = data.get("files", [])
src_files = []
test_files = []
for fi in files:
    fname = fi.get("file", "")
    if "block-time-forwarder/" in fname:
        short = "btf/" + fname.split("block-time-forwarder/")[-1]
    elif "solana-pa-prototype/programs/solana-pa-prototype/" in fname:
        short = fname.split("solana-pa-prototype/programs/solana-pa-prototype/")[-1]
    else:
        short = fname
    cov = int(fi.get("covered_lines", 0))
    tot = int(fi.get("total_lines", 0))
    pct = float(fi.get("percent_covered", "0"))
    entry = (short, cov, tot, pct)
    if "/tests/" in fname or "test" in short.split("/")[-1]:
        test_files.append(entry)
    else:
        src_files.append(entry)

src_files.sort(key=lambda r: r[3])

print()
print("Source file coverage (sorted by %, ascending):")
print("{:<55} {:>5} {:>5} {:>7}".format("File", "Cov", "Total", "Pct"))
print("-" * 75)
src_cov = src_tot = 0
for short, cov, tot, pct in src_files:
    src_cov += cov
    src_tot += tot
    print("{:<55} {:>5} {:>5} {:>6.1f}%".format(short, cov, tot, pct))
if src_tot > 0:
    print("-" * 75)
    print("{:<55} {:>5} {:>5} {:>6.1f}%".format("TOTAL (source)", src_cov, src_tot, src_cov/src_tot*100))

print()
print("Note: #[cfg(not(test))] code (cpi.rs, instruction handlers)")
print("is excluded from the test binary and invisible to kcov.")
print("Full HTML report: file://$KCOV_DIR/kcov-merged/index.html")
PYEOF
    ;;

  run)
    shift
    run_in_project "$*"
    ;;

  "")
    echo "Usage: $0 <command>"
    echo ""
    echo "Commands:"
    echo "  shell        Enter the Nix development shell"
    echo "  test         Run Rust tests"
    echo "  fmt          Check Rust formatting"
    echo "  clippy       Run clippy lints"
    echo "  anchor-build Build Anchor programs"
    echo "  anchor-test    Run deterministic Anchor integration tests"
    echo "  gen-fixtures Generate test fixtures (pass output paths as args)"
    echo "  fixture-test Run fixture-gen tests"
    echo "  validator    Start a local Solana validator"
    echo "  update-deps  Regenerate yarn.lock"
    echo "  coverage     Run unit tests with kcov and report line coverage"
    echo "  clean        Remove local validator/test artifacts"
    echo "  lock-check   Verify Cargo.lock files agree on shared git deps"
    echo "  lock-sync <pkg>  Update <pkg> in every Cargo.lock so they re-align"
    echo "  run <cmd>    Run an arbitrary command in the Nix dev shell"
    echo ""
    echo "Devnet commands:"
    echo "  devnet deploy [pa|btf|anomapay-forwarder|all]   First-time deploy to devnet"
    echo "  devnet upgrade [pa|btf|anomapay-forwarder|all]  Rebuild + deploy over existing programs"
    echo "  devnet teardown [pa|btf|anomapay-forwarder|all] PERMANENT: close programs, reclaim rent"
    echo "  devnet close-pdas            Close all PA PDA accounts, reclaim rent"
    echo "  devnet close-pa-state        Close only PAState (for re-init after upgrade)"
    echo "  devnet test                             Run integration tests against devnet"
    echo "  devnet init                             Initialize PA state (idempotent)"
    echo "  devnet init-forwarder <mint>            Initialize forwarder + escrow for token"
    echo "  devnet status                           Show deployment status + wallet balance"
    echo "  devnet balance                          Show wallet address and balance"
    exit 1
    ;;

  *)
    echo "Unknown command: $1"
    exit 1
    ;;
esac
