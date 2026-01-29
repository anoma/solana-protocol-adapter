#!/bin/bash
# Docker entrypoint script - ensures runtime initialization for volume-mounted paths
set -e

# Ensure Solana keypair exists (volumes may be empty on first run)
if [ ! -f "$HOME/.config/solana/id.json" ]; then
    echo "Generating Solana keypair..."
    mkdir -p "$HOME/.config/solana"
    solana-keygen new --no-bip39-passphrase -o "$HOME/.config/solana/id.json"
    solana config set --url localhost
fi

# Ensure validator ledger directory exists
mkdir -p "$HOME/validator-ledger"

# Execute the requested command
exec "$@"
