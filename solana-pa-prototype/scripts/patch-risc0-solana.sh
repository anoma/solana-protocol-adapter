#!/usr/bin/env bash
# Patch risc0-solana submodule with committed keypairs and program IDs.
# This ensures CI builds use the same program IDs as the fixtures.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KEYPAIRS_DIR="$SCRIPT_DIR/../keypairs/risc0-solana"
RISC0_VERIFIER="$REPO_ROOT/risc0-solana/solana-verifier"

# Ensure keypairs directory exists
if [[ ! -d "$KEYPAIRS_DIR" ]]; then
    echo "Error: Keypairs directory not found: $KEYPAIRS_DIR"
    exit 1
fi

# Create target/deploy if it doesn't exist
mkdir -p "$RISC0_VERIFIER/target/deploy"

# Copy keypairs
echo "Copying keypairs to risc0-solana..."
cp "$KEYPAIRS_DIR/groth_16_verifier-keypair.json" "$RISC0_VERIFIER/target/deploy/"
cp "$KEYPAIRS_DIR/verifier_router-keypair.json" "$RISC0_VERIFIER/target/deploy/"

# Get program IDs from keypairs
GROTH16_ID=$(solana-keygen pubkey "$RISC0_VERIFIER/target/deploy/groth_16_verifier-keypair.json")
ROUTER_ID=$(solana-keygen pubkey "$RISC0_VERIFIER/target/deploy/verifier_router-keypair.json")

echo "groth_16_verifier: $GROTH16_ID"
echo "verifier_router: $ROUTER_ID"

# Patch groth_16_verifier/src/lib.rs
GROTH16_LIB="$RISC0_VERIFIER/programs/groth_16_verifier/src/lib.rs"
echo "Patching $GROTH16_LIB..."
sed -i "s/declare_id!(\"[^\"]*\");/declare_id!(\"$GROTH16_ID\");/" "$GROTH16_LIB"

# Patch verifier_router/src/lib.rs
ROUTER_LIB="$RISC0_VERIFIER/programs/verifier_router/src/lib.rs"
echo "Patching $ROUTER_LIB..."
sed -i "s/declare_id!(\"[^\"]*\");/declare_id!(\"$ROUTER_ID\");/" "$ROUTER_LIB"

# Patch Anchor.toml
ANCHOR_TOML="$RISC0_VERIFIER/Anchor.toml"
echo "Patching $ANCHOR_TOML..."
sed -i "s/^groth_16_verifier = \"[^\"]*\"/groth_16_verifier = \"$GROTH16_ID\"/" "$ANCHOR_TOML"
sed -i "s/^verifier_router = \"[^\"]*\"/verifier_router = \"$ROUTER_ID\"/" "$ANCHOR_TOML"

echo "risc0-solana patched successfully."
