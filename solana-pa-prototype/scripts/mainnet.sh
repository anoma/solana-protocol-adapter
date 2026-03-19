#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

MAINNET_URL="${MAINNET_RPC_URL:-https://api.mainnet-beta.solana.com}"
MAINNET_WALLET="${MAINNET_WALLET:-${PROJECT_DIR}/scripts/mainnet-wallet.json}"

# Program registry — same programs as devnet.
declare -A PROGRAMS=(
  [pa]="solana_pa_prototype"
  [btf]="block_time_forwarder"
  [anomapay-forwarder]="spl_token_forwarder"
)

PA_NEEDS_INIT=true

# ---------- helpers ----------

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
  echo "⚠️  MAINNET OPERATION"
  echo "$msg"
  echo ""
  read -r -p "Type 'yes' to proceed: " answer
  if [[ "$answer" != "yes" ]]; then
    echo "Aborted."
    exit 1
  fi
}

ensure_mainnet_wallet() {
  if [[ ! -f "$MAINNET_WALLET" ]]; then
    echo "Generating mainnet wallet at ${MAINNET_WALLET}..."
    echo ""
    echo "⚠️  This wallet will control mainnet programs and hold real SOL."
    echo "Back up the keypair file immediately after generation."
    echo ""
    read -r -p "Generate now? Type 'yes': " answer
    if [[ "$answer" != "yes" ]]; then
      echo "Aborted. Create the wallet manually:"
      echo "  solana-keygen new --no-bip39-passphrase -o $MAINNET_WALLET"
      exit 1
    fi
    solana-keygen new --no-bip39-passphrase -o "$MAINNET_WALLET"
    echo ""
    echo "⚠️  BACK UP THIS FILE: $MAINNET_WALLET"
    echo "If lost, you cannot upgrade or manage your mainnet programs."
  fi
  local pubkey
  pubkey="$(solana-keygen pubkey "$MAINNET_WALLET")"
  echo "Mainnet wallet: ${pubkey}"
}

require_mainnet_wallet() {
  if [[ ! -f "$MAINNET_WALLET" ]]; then
    echo "❌ No mainnet wallet found at ${MAINNET_WALLET}"
    echo "Run: ./scripts/dev.sh mainnet deploy"
    echo "Or set MAINNET_WALLET to point to an existing keypair."
    exit 1
  fi
}

get_wallet_pubkey() {
  solana-keygen pubkey "$MAINNET_WALLET"
}

get_balance() {
  solana balance --keypair "$MAINNET_WALLET" --url "$MAINNET_URL" | awk '{print $1}'
}

require_balance() {
  local min_sol="$1"
  local balance
  balance="$(get_balance)"

  if awk "BEGIN{exit ($balance >= $min_sol) ? 0 : 1}"; then
    echo "Balance: ${balance} SOL (need ${min_sol})"
    return 0
  fi

  echo "❌ Insufficient balance: ${balance} SOL (need ${min_sol})"
  echo "Wallet: $(get_wallet_pubkey)"
  echo "Transfer SOL to this address before deploying."
  exit 1
}

get_program_id() {
  local name="$1"
  solana-keygen pubkey "target/deploy/${name}-keypair.json"
}

is_deployed() {
  local program_id="$1"
  solana program show "$program_id" --url "$MAINNET_URL" >/dev/null 2>&1
}

deploy_program() {
  local name="$1"
  local program_id
  program_id="$(get_program_id "$name")"

  echo "Deploying ${name} (${program_id})..."
  if ! solana program deploy \
    "target/deploy/${name}.so" \
    --keypair "$MAINNET_WALLET" \
    --program-id "target/deploy/${name}-keypair.json" \
    --url "$MAINNET_URL" 2>&1; then
    echo ""
    echo "❌ Deploy failed for ${name}."
    echo "If the program was previously closed, the ID is permanently burned."
    echo "To recover: delete target/deploy/${name}-keypair.json, run 'anchor build --no-idl'"
    echo "to generate a new keypair, then deploy again."
    exit 1
  fi
  echo "  ✅ ${name} deployed: ${program_id}"
  print_explorer_link "$program_id"
}

build_programs() {
  echo "Building programs..."
  anchor build --no-idl
}

init_pa() {
  echo "Initializing PA (idempotent)..."
  local program_id
  program_id="$(get_program_id "solana_pa_prototype")"

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_WALLET="$MAINNET_WALLET" \
    npx ts-node "${SCRIPT_DIR}/devnet-init-pa.ts"
}

init_forwarder() {
  local token_mint="${1:?TOKEN_MINT is required (base58 mint address)}"

  echo "Initializing forwarder (idempotent)..."

  # TOKEN_TRANSFER_ID from transfer_library — must match the backend.
  local logic_ref="8bceee49ac4646f7bf1ba20be658be5ab5699ce5cab58004f44efaa900717384"

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_PROVIDER_CLUSTER=mainnet-beta \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  LOGIC_REF="$logic_ref" \
  TOKEN_MINT="$token_mint" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/devnet-init-forwarder.ts"
}

resolve_targets() {
  local target="${1:-all}"
  case "$target" in
    all)
      echo "${!PROGRAMS[*]}"
      ;;
    pa|btf|anomapay-forwarder)
      echo "$target"
      ;;
    *)
      echo "❌ Unknown target: ${target}" >&2
      echo "Valid targets: pa, btf, anomapay-forwarder, all" >&2
      exit 1
      ;;
  esac
}

estimate_balance_needed() {
  local targets="$1"
  local total=0
  for t in $targets; do
    case "$t" in
      pa)        total=$(awk "BEGIN{print $total + 5}") ;;
      btf)       total=$(awk "BEGIN{print $total + 2}") ;;
      anomapay-forwarder) total=$(awk "BEGIN{print $total + 3}") ;;
    esac
  done
  echo "$total"
}

print_explorer_link() {
  local program_id="$1"
  echo "  https://explorer.solana.com/address/${program_id}"
}

# ---------- commands ----------

cmd_deploy() {
  local targets
  targets="$(resolve_targets "${1:-all}")"
  local min_sol
  min_sol="$(estimate_balance_needed "$targets")"

  require_cmd anchor
  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  confirm "Deploy programs to MAINNET: ${targets}
This will spend real SOL (~${min_sol} SOL) to deploy on-chain programs.
Make sure the PA is built with the correct mainnet VERIFIER_ROUTER_ID."

  ensure_mainnet_wallet
  build_programs
  require_balance "$min_sol"

  for t in $targets; do
    deploy_program "${PROGRAMS[$t]}"
  done

  if [[ "$PA_NEEDS_INIT" == "true" ]] && [[ " $targets " == *" pa "* ]]; then
    init_pa
  fi

  echo ""
  echo "✅ Mainnet deploy complete"
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    echo "  ${t}: ${pid}"
  done

  echo ""
  echo "Update MAINNET_PA_PROGRAM_ID and MAINNET_FORWARDER_PROGRAM_ID in anomapay.sh"
  echo "with the program IDs above to enable 'ANOMAPAY_NETWORK=mainnet' mode."
}

cmd_upgrade() {
  local targets
  targets="$(resolve_targets "${1:-all}")"

  require_cmd anchor
  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  # Verify target programs are already deployed
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    if ! is_deployed "$pid"; then
      echo "❌ ${t} (${pid}) is not deployed — use 'deploy' for first-time deployment"
      exit 1
    fi
  done

  confirm "Upgrade programs on MAINNET: ${targets}
This will overwrite live mainnet programs with newly-built binaries.
Make sure the PA is built with the correct mainnet VERIFIER_ROUTER_ID."

  build_programs

  for t in $targets; do
    deploy_program "${PROGRAMS[$t]}"
  done

  echo ""
  echo "✅ Mainnet upgrade complete"
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    echo "  ${t}: ${pid}"
  done
}

cmd_status() {
  require_cmd solana
  require_cmd solana-keygen

  cd "$PROJECT_DIR"

  echo "=== Mainnet Status ==="
  echo ""

  # Wallet
  if [[ -f "$MAINNET_WALLET" ]]; then
    local pubkey balance
    pubkey="$(get_wallet_pubkey)"
    balance="$(get_balance)"
    echo "Wallet: ${pubkey}"
    echo "Balance: ${balance} SOL"
    print_explorer_link "$pubkey"
  else
    echo "Wallet: not created (run deploy to generate)"
  fi
  echo ""

  # Programs
  for t in "${!PROGRAMS[@]}"; do
    local name="${PROGRAMS[$t]}"
    local keypair="target/deploy/${name}-keypair.json"
    if [[ -f "$keypair" ]]; then
      local pid
      pid="$(solana-keygen pubkey "$keypair")"
      if is_deployed "$pid"; then
        echo "${t} (${name}): ✅ deployed — ${pid}"
      else
        echo "${t} (${name}): not deployed — ${pid}"
      fi
      print_explorer_link "$pid"
    else
      echo "${t} (${name}): no keypair (run anchor build first)"
    fi
  done
  echo ""

  # PAState PDA
  if [[ -f "target/deploy/solana_pa_prototype-keypair.json" ]]; then
    local pa_pid
    pa_pid="$(get_program_id "solana_pa_prototype")"
    local pa_state
    pa_state="$(solana find-program-derived-address "$pa_pid" string:pa_state --url "$MAINNET_URL" 2>/dev/null | head -1 || true)"
    if [[ -n "$pa_state" ]]; then
      local pa_state_addr
      pa_state_addr="$(echo "$pa_state" | awk '{print $1}')"
      if solana account "$pa_state_addr" --url "$MAINNET_URL" >/dev/null 2>&1; then
        echo "PAState PDA: ✅ initialized — ${pa_state_addr}"
      else
        echo "PAState PDA: not initialized — ${pa_state_addr}"
      fi
    fi
  fi
}

cmd_init() {
  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  local pid
  pid="$(get_program_id "solana_pa_prototype")"
  if ! is_deployed "$pid"; then
    echo "❌ PA (${pid}) is not deployed on mainnet"
    echo "Run: ./scripts/dev.sh mainnet deploy pa"
    exit 1
  fi

  confirm "Initialize PA state on MAINNET.
This creates the PAState PDA and genesis root marker."

  init_pa
}

cmd_init_forwarder() {
  local token_mint="${1:-}"
  if [[ -z "$token_mint" ]]; then
    echo "❌ TOKEN_MINT argument is required"
    echo "Usage: mainnet.sh init-forwarder <token_mint>"
    echo "Mainnet USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    exit 1
  fi

  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  local pid
  pid="$(get_program_id "spl_token_forwarder")"
  if ! is_deployed "$pid"; then
    echo "❌ Forwarder (${pid}) is not deployed on mainnet"
    echo "Run: ./scripts/dev.sh mainnet deploy anomapay-forwarder"
    exit 1
  fi

  confirm "Initialize forwarder on MAINNET with token mint: ${token_mint}
This creates the forwarder config PDA and escrow ATA."

  init_forwarder "$token_mint"
}

cmd_balance() {
  require_cmd solana
  require_cmd solana-keygen

  if [[ ! -f "$MAINNET_WALLET" ]]; then
    echo "Wallet: not created (run deploy to generate)"
    return 0
  fi

  local pubkey balance
  pubkey="$(get_wallet_pubkey)"
  balance="$(get_balance)"
  echo "${pubkey}  ${balance} SOL"
}

# ---------- dispatch ----------

case "${1:-}" in
  deploy)
    cmd_deploy "${2:-all}"
    ;;
  upgrade)
    cmd_upgrade "${2:-all}"
    ;;
  status)
    cmd_status
    ;;
  init)
    cmd_init
    ;;
  init-forwarder)
    cmd_init_forwarder "${2:-}"
    ;;
  balance)
    cmd_balance
    ;;
  *)
    echo "Usage: mainnet.sh <command> [target]"
    echo ""
    echo "Commands:"
    echo "  deploy [pa|btf|anomapay-forwarder|all]   First-time deploy to mainnet (default: all)"
    echo "  upgrade [pa|btf|anomapay-forwarder|all]  Rebuild + deploy over existing programs"
    echo "  init                            Initialize PA state (idempotent)"
    echo "  init-forwarder <mint>           Initialize forwarder + escrow for token mint"
    echo "  status                          Show deployment status + wallet balance"
    echo "  balance                         Show wallet address and balance"
    echo ""
    echo "Mainnet USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    echo ""
    echo "Environment variables:"
    echo "  MAINNET_RPC_URL   RPC endpoint (default: https://api.mainnet-beta.solana.com)"
    echo "  MAINNET_WALLET    Keypair file (default: scripts/mainnet-wallet.json)"
    exit 1
    ;;
esac
