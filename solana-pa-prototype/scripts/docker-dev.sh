#!/bin/bash
# Helper script for Docker development environment

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

ensure_dind() {
    ./scripts/docker.sh compose up -d dind
    for i in {1..60}; do
        if ./scripts/docker.sh compose exec -T dind docker info >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "ERROR: Docker daemon did not become ready." >&2
    exit 1
}

ensure_dev_image() {
    local host_uid dev_uid
    host_uid="$(id -u)"
    dev_uid="$(./scripts/docker.sh compose run --rm dev id -u 2>/dev/null || echo "")"
    if [[ -z "$dev_uid" || "$dev_uid" != "$host_uid" ]]; then
        ./scripts/docker.sh compose build dev
    fi
}

case "$1" in
    build)
        echo "Building Docker image..."
        ./scripts/docker.sh compose build dev
        ;;

    shell)
        echo "Starting development shell..."
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm --service-ports dev
        ;;

    validator)
        echo "Starting Solana test validator..."
        ./scripts/docker.sh compose --profile validator up validator
        ;;

    test)
        echo "Running tests in container..."
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm dev bash -c "cd /workspace/solana-pa-prototype && cargo test --workspace"
        ;;

    anchor-build)
        echo "Building Anchor programs..."
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm dev bash -c "cd /workspace/solana-pa-prototype && anchor build"
        ;;

    anchor-test)
        echo "Running Anchor tests (deterministic)..."
        ./scripts/docker-anchor-test.sh
        ;;

    build-verifier)
        echo "Building risc0-solana verifier..."
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm dev bash -c "cd /workspace/risc0-solana/solana-verifier && anchor build"
        ;;

    full-test)
        echo "Running full integration test..."
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm --service-ports dev bash -c "
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
        ensure_dind
        ensure_dev_image
        ./scripts/docker.sh compose run --rm dev bash -c "cd /workspace/solana-pa-prototype && rm -f yarn.lock package-lock.json && yarn install"
        ;;

    clean)
        echo "Cleaning up..."
        ./scripts/docker.sh compose down -v
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
