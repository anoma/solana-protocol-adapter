#!/usr/bin/env bash
# Verify RISC0 verifier infrastructure is available for localnet testing.
#
# The verifier programs AND their initialized state are automatically cloned
# from devnet when solana-test-validator starts (via Anchor.toml or docker-compose.yml).
# This script only verifies the cloned state is correct.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PA_ROOT="$SCRIPT_DIR/.."

# Use Anchor's provider URL if available, otherwise fall back to CLUSTER_URL or localhost
CLUSTER_URL="${ANCHOR_PROVIDER_URL:-${CLUSTER_URL:-http://localhost:8899}}"

# Devnet program addresses (cloned by solana-test-validator)
VERIFIER_ROUTER_ID="CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq"
GROTH16_VERIFIER_ID="DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk"

# Devnet state PDAs (cloned by solana-test-validator)
ROUTER_PDA="5GzjEjtL3JqKSp8sx4Kjzxuwg6xQ3pPyuTqnQJxbemEk"
VERIFIER_ENTRY_PDA="DMXuNWRSEVjJnfDovkQLdgmoSi1HBRvG7svJUEBqfz3F"

echo "==> Verifying RISC0 verifier infrastructure"
echo "    Cluster URL: $CLUSTER_URL"

# Configure Solana CLI
solana config set --url "$CLUSTER_URL" >/dev/null

# Verify programs were cloned from devnet
echo "==> Verifying cloned programs"
if ! solana program show "$VERIFIER_ROUTER_ID" >/dev/null 2>&1; then
    echo "ERROR: Verifier router program not found at $VERIFIER_ROUTER_ID"
    echo "       Ensure solana-test-validator was started with --clone-upgradeable-program"
    exit 1
fi
if ! solana program show "$GROTH16_VERIFIER_ID" >/dev/null 2>&1; then
    echo "ERROR: Groth16 verifier program not found at $GROTH16_VERIFIER_ID"
    echo "       Ensure solana-test-validator was started with --clone-upgradeable-program"
    exit 1
fi
echo "    Router program:  $VERIFIER_ROUTER_ID ✓"
echo "    Groth16 program: $GROTH16_VERIFIER_ID ✓"

# Verify state PDAs were cloned from devnet
echo "==> Verifying cloned state"
if ! solana account "$ROUTER_PDA" >/dev/null 2>&1; then
    echo "ERROR: Router PDA not found at $ROUTER_PDA"
    echo "       Ensure solana-test-validator was started with --clone $ROUTER_PDA"
    exit 1
fi
if ! solana account "$VERIFIER_ENTRY_PDA" >/dev/null 2>&1; then
    echo "ERROR: Verifier entry PDA not found at $VERIFIER_ENTRY_PDA"
    echo "       Ensure solana-test-validator was started with --clone $VERIFIER_ENTRY_PDA"
    exit 1
fi
echo "    Router PDA:         $ROUTER_PDA ✓"
echo "    Verifier Entry PDA: $VERIFIER_ENTRY_PDA ✓"

echo "==> RISC0 verifier infrastructure ready"
echo "    Router:  $VERIFIER_ROUTER_ID"
echo "    Groth16: $GROTH16_VERIFIER_ID"
