#!/bin/bash
# Helper script for Docker development environment

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

# Build compose command: add GPU override when USE_GPU=1
COMPOSE="docker compose"
if [[ "${USE_GPU:-}" == "1" ]]; then
  COMPOSE="docker compose -f docker-compose.yml -f docker-compose.gpu.yml"
fi

case "$1" in
    build)
        echo "Building Docker image..."
        $COMPOSE build dev
        ;;

    shell)
        echo "Starting development shell..."
        $COMPOSE run --rm --service-ports dev
        ;;

    validator)
        echo "Starting Solana test validator..."
        $COMPOSE --profile validator up validator
        ;;

    test)
        echo "Running tests in container..."
        $COMPOSE run --rm dev bash -c "cd /workspace/solana-pa-prototype && cargo test --workspace"
        ;;

    anchor-build)
        echo "Building Anchor programs..."
        $COMPOSE run --rm dev bash -c "cd /workspace/solana-pa-prototype && anchor build"
        ;;

    anchor-test)
        echo "Running Anchor tests (deterministic)..."
        ./scripts/docker-anchor-test.sh
        ;;

    build-verifier)
        echo "Building risc0-solana verifier..."
        $COMPOSE run --rm dev bash -c "cd /workspace/risc0-solana/solana-verifier && anchor build"
        ;;

    full-test)
        echo "Running full integration test..."
        $COMPOSE run --rm --service-ports dev bash -c "
            cd /workspace/solana-pa-prototype
            echo '=== Building PA ==='
            anchor build

            echo '=== Building risc0-solana verifier ==='
            cd /workspace/risc0-solana/solana-verifier
            anchor build

            echo '=== Starting validator and running tests ==='
            cd /workspace/solana-pa-prototype
            anchor test
        "
        ;;

    update-deps)
        echo "Regenerating yarn.lock inside Docker..."
        $COMPOSE run --rm dev bash -c "cd /workspace/solana-pa-prototype && rm -f yarn.lock package-lock.json && yarn install"
        ;;

    clean)
        echo "Cleaning up..."
        $COMPOSE down -v
        ;;

    *)
        echo "Solana PA Docker Development Helper"
        echo ""
        echo "Usage: $0 <command>"
        echo ""
        echo "Commands:"
        echo "  build          Build the Docker image"
        echo "  shell          Start an interactive development shell"
        echo "  validator      Start Solana test validator"
        echo "  test           Run Rust tests"
        echo "  anchor-build   Build Anchor programs"
        echo "  anchor-test    Run Anchor integration tests"
        echo "  build-verifier Build risc0-solana verifier"
        echo "  full-test      Run full integration test suite"
        echo "  update-deps    Update yarn.lock when package.json changes"
        echo "  clean          Stop and remove containers/volumes"
        ;;
esac
