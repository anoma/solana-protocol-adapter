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
    ;;

  anchor-test)
    run_in_project "./scripts/anchor-test.sh"
    ;;

  validator)
    run_in_project "solana-test-validator --reset --url devnet --rpc-port 8899 --faucet-port 9900 --bind-address 127.0.0.1 --log"
    ;;

  gen-fixtures)
    shift
    run_in_project "cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- $*"
    ;;

  fixture-test)
    run_in_project "RISC0_SKIP_BUILD=1 cargo test --manifest-path tools/fixture-gen/Cargo.toml"
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
    echo "  anchor-test  Run deterministic Anchor integration tests"
    echo "  gen-fixtures Generate test fixtures (pass output paths as args)"
    echo "  fixture-test Run fixture-gen tests"
    echo "  validator    Start a local Solana validator"
    echo "  update-deps  Regenerate yarn.lock"
    echo "  coverage     Run unit tests with kcov and report line coverage"
    echo "  clean        Remove local validator/test artifacts"
    echo "  run <cmd>    Run an arbitrary command in the Nix dev shell"
    echo ""
    echo "Devnet commands:"
    echo "  devnet deploy [pa|btf|all]    First-time deploy to devnet"
    echo "  devnet upgrade [pa|btf|all]   Rebuild + deploy over existing programs"
    echo "  devnet teardown [pa|btf|all]  PERMANENT: close programs, reclaim rent"
    echo "  devnet close-pdas            Close all PA PDA accounts, reclaim rent"
    echo "  devnet close-pa-state        Close only PAState (for re-init after upgrade)"
    echo "  devnet test                   Run integration tests against devnet"
    echo "  devnet init                   Initialize PA state (idempotent)"
    echo "  devnet status                 Show deployment status + wallet balance"
    echo "  devnet balance                Show wallet address and balance"
    exit 1
    ;;

  *)
    echo "Unknown command: $1"
    exit 1
    ;;
esac
