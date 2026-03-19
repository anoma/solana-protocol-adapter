#!/usr/bin/env bash
set -euo pipefail

# Deploy RISC0 verifier infrastructure (verifier router + groth16 verifier).
#
# This clones risc0-solana v3.0.0, builds both programs, deploys them,
# initializes the router PDA, and registers the groth16 verifier.
#
# After deployment, update VERIFIER_ROUTER_ID in
#   programs/solana-pa-prototype/src/lib.rs
# with the new router program ID and rebuild the PA.
#
# Usage:
#   ./scripts/deploy-verifier.sh devnet          # deploy to devnet
#   ./scripts/deploy-verifier.sh mainnet         # deploy to mainnet
#   ./scripts/deploy-verifier.sh devnet status   # check deployed programs
#
# Environment variables:
#   DEVNET_RPC_URL    Override devnet RPC (default: https://api.devnet.solana.com)
#   MAINNET_RPC_URL   Override mainnet RPC (default: https://api.mainnet-beta.solana.com)
#   MAINNET_WALLET    Override mainnet wallet path

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

RISC0_SOLANA_TAG="v3.0.0"
RISC0_SOLANA_REPO="https://github.com/risc0/risc0-solana.git"
CACHE_DIR="${PROJECT_DIR}/.cache/risc0-solana"
VERIFIER_DIR="${CACHE_DIR}/solana-verifier"

# Groth16 selector — must match the proof fixtures.
SELECTOR="0x73c457ba"

# ── Network configuration ────────────────────────────────────────────────

NETWORK="${1:-}"

case "$NETWORK" in
  devnet)
    RPC_URL="${DEVNET_RPC_URL:-https://api.devnet.solana.com}"
    RPC_WS="${DEVNET_RPC_WS:-wss://api.devnet.solana.com}"
    WALLET="${PROJECT_DIR}/scripts/devnet-wallet.json"
    EXPLORER_SUFFIX="?cluster=devnet"
    ;;
  mainnet)
    RPC_URL="${MAINNET_RPC_URL:-https://api.mainnet-beta.solana.com}"
    RPC_WS="${MAINNET_RPC_WS:-wss://api.mainnet-beta.solana.com}"
    WALLET="${MAINNET_WALLET:-${PROJECT_DIR}/scripts/mainnet-wallet.json}"
    EXPLORER_SUFFIX=""
    ;;
  *)
    echo "Usage: deploy-verifier.sh <devnet|mainnet> [deploy|status]"
    echo ""
    echo "Commands:"
    echo "  deploy (default)   Build, deploy, and initialize verifier infrastructure"
    echo "  status             Show deployed verifier program status"
    echo ""
    echo "This deploys the RISC0 verifier router and groth16 verifier programs."
    echo "After deployment, update VERIFIER_ROUTER_ID in lib.rs and rebuild the PA."
    exit 1
    ;;
esac

COMMAND="${2:-deploy}"

# ── Helpers ───────────────────────────────────────────────────────────────

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "❌ Missing required command: $1"
    echo "Run this inside the Nix dev shell: nix develop"
    exit 1
  fi
}

confirm() {
  local msg="$1"
  echo ""
  echo "⚠️  ${NETWORK^^} OPERATION"
  echo "$msg"
  echo ""
  read -r -p "Type 'yes' to proceed: " answer
  if [[ "$answer" != "yes" ]]; then
    echo "Aborted."
    exit 1
  fi
}

require_wallet() {
  if [[ ! -f "$WALLET" ]]; then
    echo "❌ No wallet found at ${WALLET}"
    if [[ "$NETWORK" == "devnet" ]]; then
      echo "Run: ./scripts/dev.sh devnet deploy  (creates the devnet wallet)"
    else
      echo "Run: ./scripts/dev.sh mainnet deploy  (creates the mainnet wallet)"
    fi
    exit 1
  fi
}

get_wallet_pubkey() {
  solana-keygen pubkey "$WALLET"
}

get_balance() {
  solana balance --keypair "$WALLET" --url "$RPC_URL" | awk '{print $1}'
}

print_explorer_link() {
  local addr="$1"
  echo "  https://explorer.solana.com/address/${addr}${EXPLORER_SUFFIX}"
}

# ── Clone & setup ─────────────────────────────────────────────────────────

ensure_risc0_solana() {
  if [[ -d "$CACHE_DIR" ]]; then
    echo "Using cached risc0-solana at ${CACHE_DIR}"
    # Verify correct tag
    local current_tag
    current_tag="$(cd "$CACHE_DIR" && git describe --tags --exact-match 2>/dev/null || echo "unknown")"
    if [[ "$current_tag" != "$RISC0_SOLANA_TAG" ]]; then
      echo "  Cached version is '${current_tag}', need '${RISC0_SOLANA_TAG}'. Re-cloning..."
      rm -rf "$CACHE_DIR"
    fi
  fi

  if [[ ! -d "$CACHE_DIR" ]]; then
    echo "Cloning risc0-solana ${RISC0_SOLANA_TAG}..."
    mkdir -p "$(dirname "$CACHE_DIR")"
    git clone --depth 1 --branch "$RISC0_SOLANA_TAG" "$RISC0_SOLANA_REPO" "$CACHE_DIR"
  fi

  # Install node dependencies
  if [[ ! -d "${VERIFIER_DIR}/node_modules" ]]; then
    echo "Installing risc0-solana dependencies..."
    (cd "$VERIFIER_DIR" && yarn install --frozen-lockfile 2>/dev/null || yarn install)
  fi
}

# ── Status ────────────────────────────────────────────────────────────────

cmd_status() {
  require_cmd solana
  require_cmd solana-keygen

  echo "=== Verifier Infrastructure Status (${NETWORK}) ==="
  echo ""

  # Wallet
  if [[ -f "$WALLET" ]]; then
    local pubkey balance
    pubkey="$(get_wallet_pubkey)"
    balance="$(get_balance)"
    echo "Wallet: ${pubkey} (${balance} SOL)"
  else
    echo "Wallet: not found at ${WALLET}"
  fi
  echo ""

  # Check for keypairs in the risc0-solana build output
  local router_keypair="${VERIFIER_DIR}/target/deploy/verifier_router-keypair.json"
  local groth16_keypair="${VERIFIER_DIR}/target/deploy/groth_16_verifier-keypair.json"

  for name_keypair in "verifier_router:${router_keypair}" "groth_16_verifier:${groth16_keypair}"; do
    local name="${name_keypair%%:*}"
    local keypair="${name_keypair#*:}"

    if [[ -f "$keypair" ]]; then
      local pid
      pid="$(solana-keygen pubkey "$keypair")"
      if solana program show "$pid" --url "$RPC_URL" >/dev/null 2>&1; then
        local info
        info="$(solana program show "$pid" --url "$RPC_URL" 2>&1)"
        local data_len balance authority
        data_len="$(echo "$info" | grep 'Data Length' | awk '{print $3}')"
        balance="$(echo "$info" | grep 'Balance' | awk '{print $2}')"
        authority="$(echo "$info" | grep 'Authority' | awk '{print $2}')"
        echo "${name}: ✅ deployed"
        echo "  Program ID: ${pid}"
        echo "  Authority:  ${authority}"
        echo "  Data:       ${data_len} bytes, ${balance} SOL"
        print_explorer_link "$pid"
      else
        echo "${name}: not deployed (${pid})"
      fi
    else
      echo "${name}: no keypair (not yet built)"
    fi
  done
}

# ── Deploy ────────────────────────────────────────────────────────────────

cmd_deploy() {
  require_cmd anchor
  require_cmd solana
  require_cmd solana-keygen
  require_cmd yarn
  require_cmd node
  require_cmd git

  require_wallet

  local pubkey balance
  pubkey="$(get_wallet_pubkey)"
  balance="$(get_balance)"

  confirm "Deploy RISC0 verifier infrastructure to ${NETWORK}.
Wallet:   ${pubkey} (${balance} SOL)
RPC:      ${RPC_URL}
Selector: ${SELECTOR}

This will:
  1. Clone and build risc0-solana ${RISC0_SOLANA_TAG}
  2. Deploy verifier_router (non-upgradeable)
  3. Deploy groth_16_verifier (upgradeable, authority transferred to router PDA)
  4. Initialize router PDA
  5. Register groth16 verifier with selector ${SELECTOR}

Estimated cost: ~3.7 SOL (router ~2.2 + groth16 ~1.4 + fees)"

  ensure_risc0_solana

  echo ""
  echo "=== Building and deploying verifier programs ==="
  echo ""

  # Configure Anchor.toml for the target network
  # The risc0 deploy script uses `anchor build` and `anchor deploy`,
  # which read provider config from Anchor.toml.
  local anchor_toml="${VERIFIER_DIR}/Anchor.toml"
  # Temporarily override provider settings
  sed -i "s|^cluster = .*|cluster = \"${NETWORK}\"|" "$anchor_toml"
  sed -i "s|^wallet = .*|wallet = \"${WALLET}\"|" "$anchor_toml"

  # Run the risc0-solana deploy script
  (
    cd "$VERIFIER_DIR"
    RPC="$RPC_URL" \
    RPC_SUBSCRIPTION="$RPC_WS" \
    KEY_PAIR_FILE="$WALLET" \
    SELECTOR="$SELECTOR" \
    MINIMUM_DEPLOY_BALANCE=4 \
    MINIMUM_BALANCE=0.5 \
      yarn run ts-node scripts/deploy.ts
  )

  echo ""
  echo "=== Deployment complete ==="
  echo ""

  # Read deployed program IDs from keypairs
  local router_id groth16_id
  router_id="$(solana-keygen pubkey "${VERIFIER_DIR}/target/deploy/verifier_router-keypair.json")"
  groth16_id="$(solana-keygen pubkey "${VERIFIER_DIR}/target/deploy/groth_16_verifier-keypair.json")"

  echo "Verifier Router: ${router_id}"
  print_explorer_link "$router_id"
  echo "Groth16 Verifier: ${groth16_id}"
  print_explorer_link "$groth16_id"

  # Derive PDAs for reference
  echo ""
  echo "=== Next steps ==="
  echo ""
  echo "1. Update VERIFIER_ROUTER_ID in programs/solana-pa-prototype/src/lib.rs:"
  echo "   const VERIFIER_ROUTER_ID: Pubkey ="
  echo "       anchor_lang::solana_program::pubkey!(\"${router_id}\");"
  echo ""
  echo "2. Rebuild and redeploy the PA:"
  echo "   ./scripts/dev.sh ${NETWORK} upgrade pa"
  echo ""
  echo "3. Update validator-deploy.sh clone addresses (for localnet):"
  echo "   VERIFIER_ROUTER=\"${router_id}\""
  echo "   GROTH16_VERIFIER=\"${groth16_id}\""
  echo ""
  echo "4. Update scripts/verifier-utils/index.ts:"
  echo "   export const VERIFIER_ROUTER_ID = new PublicKey(\"${router_id}\");"
  echo "   export const GROTH16_VERIFIER_ID = new PublicKey(\"${groth16_id}\");"
}

# ── Dispatch ──────────────────────────────────────────────────────────────

case "$COMMAND" in
  deploy)
    cmd_deploy
    ;;
  status)
    cmd_status
    ;;
  *)
    echo "Unknown command: ${COMMAND}"
    echo "Usage: deploy-verifier.sh <devnet|mainnet> [deploy|status]"
    exit 1
    ;;
esac
