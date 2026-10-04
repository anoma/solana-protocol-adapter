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

# Shared library: lockfile-sync checks (LOCK_FILES, ensure_lockfile_sync)
# and program build/registry functions. Sourcing has no side effects beyond
# variable defaults, so it is safe outside the Nix shell.
# shellcheck source=validator-deploy.sh
source "${SCRIPT_DIR}/validator-deploy.sh"

# Every command here builds or runs against the local addresses; cluster
# operations (dispatched to ops.sh) load their cluster's on top.
load_program_ids localnet

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
    run_in_project "cargo test --workspace"
    ;;

  anchor-build)
    # Development compile check: dev-teardown enabled, no IDL generation.
    # The production build is `release-build` below.
    run_in_project "./scripts/ops.sh build-dev --no-idl"
    ;;

  release-build)
    # Production build: no dev features; verifies each production IDL is the
    # development IDL minus the declared dev-only instructions.
    run_in_project "./scripts/ops.sh build-release"
    ;;

  anchor-test)
    # ops.sh owns the dispatch: no --cluster (or --cluster localnet) runs the
    # full deterministic local flow; devnet/mainnet runs the cluster-safe
    # subset against the programs already deployed there.
    shift
    run_in_project "./scripts/ops.sh $(printf '%q ' test "$@")"
    ;;

  deploy|upgrade|init|set-kind-table|deny-logic-ref|forwarder|lookup-table|pause|unpause|status|balance|idl-publish|verify-build|refresh-devnet-programs)
    # Cluster operations — see ./scripts/ops.sh for flags and semantics.
    run_in_project "./scripts/ops.sh $(printf '%q ' "$@")"
    ;;

  validator)
    # ops.sh validator uses start_validator (validator-deploy.sh), which loads
    # the devnet programs and preloads the marker fixtures — a bare validator
    # cannot settle anything.
    run_in_project "./scripts/ops.sh validator"
    ;;

  validator-deploy)
    # Like validator, but also builds and deploys all programs first and
    # keeps the validator running for external clients (test harnesses).
    run_in_project "./scripts/ops.sh validator-deploy"
    ;;

  gen-fixtures)
    shift
    ensure_lockfile_sync
    run_in_project "cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- $*"
    ;;

  regen-fixtures)
    # Regenerate the COMPLETE fixture set for one proof mode, sequentially.
    shift
    ensure_lockfile_sync
    run_in_project "./scripts/regen-fixtures.sh $(printf '%q ' "$@")"
    ;;

  lock-sync)
    shift
    sync_lockfiles_for_package "${1:-}"
    ;;

  lock-check)
    ensure_lockfile_sync && echo "All Cargo.lock files are in sync."
    ;;

  fixture-test)
    # The fixture-gen test module documents requiring RISC0_DEV_MODE=1: its
    # transaction-building tests execute guests without proving. Without it
    # they run real succinct proving in parallel and OOM the machine.
    run_in_project "RISC0_DEV_MODE=1 RISC0_SKIP_BUILD=1 cargo test --manifest-path tools/fixture-gen/Cargo.toml"
    ;;

  update-deps)
    run_in_project "rm -f yarn.lock package-lock.json && yarn install"
    ;;

  clean)
    run_in_project "rm -rf .validator-ledger .validator.log test-ledger"
    ;;

  fmt)
    run_in_project "cargo fmt --all -- --check && cargo fmt --manifest-path tools/fixture-gen/Cargo.toml --all -- --check"
    ;;

  clippy)
    run_in_project "./scripts/ops.sh clippy && cargo clippy --manifest-path tools/fixture-gen/Cargo.toml --all-targets -- -D warnings"
    ;;

  coverage)
    echo "Building test binaries..."
    BUILD_JSON=$(run_in_project "cargo test --workspace --no-run --message-format=json")
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
    COV_JSON=$(find "$KCOV_DIR" -name coverage.json -path "*/kcov-merged/*" -print -quit)
    if [[ -z "$COV_JSON" ]]; then
      COV_JSON=$(find "$KCOV_DIR" -name coverage.json -print -quit)
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
    short = fname.split("/programs/")[-1]
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
    echo "  anchor-build Build Anchor programs (development build, dev-teardown enabled)"
    echo "  release-build Build the production binaries (no dev features; verifies each"
    echo "               IDL is the development IDL minus the declared dev-only instructions)"
    echo "  anchor-test [--cluster <c>] [--mode <real|mock>] [--prebuilt] [spec file...]"
    echo "               Local: full deterministic integration flow (default),"
    echo "               every spec file on one validator; spec files"
    echo "               (e.g. tests/settle.ts) restrict the run."
    echo "               devnet/mainnet, or --prebuilt: cluster-safe subset against"
    echo "               the programs already deployed there"
    echo "  gen-fixtures <shape> [options] OUT"
    echo "               Generate one fixture (gen-fixtures --help lists the shapes)"
    echo "  regen-fixtures <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH]"
    echo "               Regenerate the complete fixture set for one proof mode"
    echo "               (sequential; real mode is hours of CPU proving)"
    echo "  fixture-test Run fixture-gen tests"
    echo "  validator    Start a local Solana validator (devnet programs only)"
    echo "  validator-deploy Build, start a validator with every program loaded"
    echo "               at genesis, and keep it running"
    echo "  refresh-devnet-programs --url <rpc>"
    echo "               Replace the committed copy of the devnet programs"
    echo "               (devnet-programs/) with devnet's current state"
    echo "  update-deps  Regenerate yarn.lock"
    echo "  coverage     Run unit tests with kcov and report line coverage"
    echo "  clean        Remove local validator/test artifacts"
    echo "  lock-check   Verify Cargo.lock files agree on shared git deps"
    echo "  lock-sync <pkg>  Update <pkg> in every Cargo.lock so they re-align"
    echo "  run <cmd>    Run an arbitrary command in the Nix dev shell"
    echo ""
    echo "Cluster operations (take --cluster <localnet|devnet|mainnet>, optional for"
    echo "verify-build; program addresses come from env/<cluster>.env; see"
    echo "./scripts/ops.sh for all flags, wallet defaults, and required env):"
    echo "  deploy [${DEPLOY_TARGETS}|all]    First-time deploy (production build; --dev-teardown opts in)"
    echo "  upgrade [${DEPLOY_TARGETS}|all]   Rebuild + deploy over existing programs"
    echo "  init                   Initialize PA state (idempotent; needs PA_OWNER,"
    echo "                         PA_VERIFIER_ROUTER and PA_PROOF_SELECTOR)"
    echo "  set-kind-table         Replace the PA's kind-table commitment (PA_KIND_TABLE_COMMITMENT)"
    echo "  deny-logic-ref         Deny a logic ref for good (PA_DENIED_LOGIC_REF)"
    echo "  lookup-table           Create/extend the deployment's settlement lookup table"
    echo "  forwarder <cmd>        SPL token forwarder operations (init, reinitialize,"
    echo "                         emergency-withdraw; STF_* env)"
    echo "  idl-publish            Publish the production IDL on chain"
    echo "  verify-build           Deterministic solana-verify build of the PA; with"
    echo "                         --cluster, compares against the deployed program"
    echo "  pause / unpause        Pause or resume settlement (owner-only)"
    echo "  status                 Show deployment status + wallet balance"
    echo "  balance                Show wallet address and balance"
    exit 1
    ;;

  *)
    echo "Unknown command: $1"
    exit 1
    ;;
esac
