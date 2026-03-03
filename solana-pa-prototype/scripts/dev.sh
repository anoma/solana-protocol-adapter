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
    run_in_project "cargo test --workspace"
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
    echo "  clean        Remove local validator/test artifacts"
    echo "  run <cmd>    Run an arbitrary command in the Nix dev shell"
    exit 1
    ;;

  *)
    echo "Unknown command: $1"
    exit 1
    ;;
esac
