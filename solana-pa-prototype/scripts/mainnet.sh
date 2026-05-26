#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=token-transfer-logic-ref.sh
source "${SCRIPT_DIR}/token-transfer-logic-ref.sh"

MAINNET_URL="${MAINNET_RPC_URL:-https://api.mainnet-beta.solana.com}"
MAINNET_WALLET="${MAINNET_WALLET:-${PROJECT_DIR}/scripts/mainnet-wallet.json}"

# Program registry — btf excluded (test utility, not needed on mainnet).
declare -A PROGRAMS=(
  [pa]="solana_pa_prototype"
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
  if [[ "${MAINNET_AUTO_CONFIRM:-}" == "1" ]]; then
    echo "[MAINNET_AUTO_CONFIRM=1 — skipping interactive prompt]"
    return 0
  fi
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

  local verifier_router="${MAINNET_VERIFIER_ROUTER:-}"
  if [[ -z "$verifier_router" ]]; then
    # Check for keypair from a previous deploy-verifier run
    local router_keypair="${PROJECT_DIR}/.cache/risc0-solana/solana-verifier/target/deploy/verifier_router-keypair.json"
    if [[ -f "$router_keypair" ]]; then
      verifier_router="$(solana-keygen pubkey "$router_keypair")"
      echo "  Using verifier router from cached keypair: $verifier_router"
    else
      echo "❌ MAINNET_VERIFIER_ROUTER not set and no cached verifier keypair found."
      echo "Run: ./scripts/dev.sh deploy-verifier mainnet"
      echo "Or set: export MAINNET_VERIFIER_ROUTER=<router_program_id>"
      exit 1
    fi
  fi

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  VERIFIER_ROUTER_PROGRAM="$verifier_router" \
    npx ts-node "${SCRIPT_DIR}/devnet-init-pa.ts"
}

init_forwarder() {
  local token_mint="${1:?TOKEN_MINT is required (base58 mint address)}"

  echo "Initializing forwarder (idempotent)..."

  require_token_transfer_logic_ref

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_PROVIDER_CLUSTER=mainnet-beta \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  LOGIC_REF="$TOKEN_TRANSFER_LOGIC_REF" \
  TOKEN_MINT="$token_mint" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/devnet-init-forwarder.ts"
}

close_forwarder_config() {
  require_token_transfer_logic_ref

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_PROVIDER_CLUSTER=mainnet-beta \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  TARGET_LOGIC_REF="$TOKEN_TRANSFER_LOGIC_REF" \
  EXPECTED_FORWARDER_PROGRAM_ID="${MAINNET_FORWARDER_PROGRAM_ID:-}" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/close-forwarder-config.ts"
}

pa_state_account_exists() {
  local pa_pid
  pa_pid="$(get_program_id "solana_pa_prototype")"
  local pa_state_addr
  pa_state_addr="$(solana find-program-derived-address "$pa_pid" string:pa_state --url "$MAINNET_URL" 2>/dev/null | head -1 | awk '{print $1}')"
  [[ -n "$pa_state_addr" ]] && solana account "$pa_state_addr" --url "$MAINNET_URL" >/dev/null 2>&1
}

# Call emergency_stop on the PA. Idempotent: AlreadyStopped is treated as success
# so callers (full-reset, close-pdas precheck) don't need to know the lifecycle.
stop_pa() {
  echo "Calling emergency_stop on PA..."
  local output rc
  set +e
  output=$(ANCHOR_PROVIDER_URL="$MAINNET_URL" \
    ANCHOR_WALLET="$MAINNET_WALLET" \
    npx ts-node "${SCRIPT_DIR}/stop-pa.ts" 2>&1)
  rc=$?
  set -e
  echo "$output"
  if [[ $rc -ne 0 ]]; then
    if echo "$output" | grep -qE "AlreadyStopped|already stopped"; then
      echo "  (PA was already stopped — proceeding)"
      return 0
    fi
    return $rc
  fi
}

recover_forwarder_escrow() {
  local token_mint="${1:?TOKEN_MINT is required (base58 mint address)}"
  local recipient_owner="${2:?RECIPIENT_OWNER is required (base58 wallet address)}"

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_PROVIDER_CLUSTER=mainnet-beta \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  TOKEN_MINT="$token_mint" \
  RECIPIENT_OWNER="$recipient_owner" \
  EXPECTED_FORWARDER_PROGRAM_ID="${MAINNET_FORWARDER_PROGRAM_ID:-}" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/recover-forwarder-escrow.ts"
}

resolve_targets() {
  local target="${1:-all}"
  case "$target" in
    all)
      echo "${!PROGRAMS[*]}"
      ;;
    pa|anomapay-forwarder)
      echo "$target"
      ;;
    *)
      echo "❌ Unknown target: ${target}" >&2
      echo "Valid targets: pa, anomapay-forwarder, all" >&2
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
This will spend real SOL (~${min_sol} SOL) to deploy on-chain programs."

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
  echo "Set MAINNET_PA_PROGRAM_ID and MAINNET_FORWARDER_PROGRAM_ID in your mainnet env file"
  echo "before running './scripts/anomapay.sh demo setup'."
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
This will overwrite live mainnet programs with newly-built binaries."

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

cmd_reset_forwarder_config() {
  local token_mint="${1:-}"
  if [[ -z "$token_mint" ]]; then
    echo "❌ TOKEN_MINT argument is required"
    echo "Usage: mainnet.sh reset-forwarder-config <token_mint>"
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

  confirm "Reset forwarder config on MAINNET for token mint: ${token_mint}
This closes only the forwarder Config PDA if its logic_ref is not the target
value, then re-initializes the forwarder config with the target logic_ref.
It does not close escrow accounts, drain tokens, close nonce bitmaps, upgrade
programs, or touch PA state."

  close_forwarder_config
  init_forwarder "$token_mint"
}

cmd_recover_forwarder_escrow() {
  local token_mint="${1:-}"
  local recipient_owner="${2:-}"
  if [[ -z "$token_mint" || -z "$recipient_owner" ]]; then
    echo "❌ TOKEN_MINT and RECIPIENT_OWNER arguments are required"
    echo "Usage: mainnet.sh recover-forwarder-escrow <token_mint> <recipient_owner>"
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

  confirm "Recover one forwarder escrow on MAINNET.
Token mint:      ${token_mint}
Recipient owner: ${recipient_owner}

This drains the entire token escrow for that mint to the recipient owner,
closes that escrow ATA, then recreates an empty escrow ATA. It does not close
the forwarder config, close nonce bitmaps, change logic_ref, upgrade programs,
or touch PA state."

  recover_forwarder_escrow "$token_mint" "$recipient_owner"
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

cmd_close_pdas() {
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  # close-pdas's marker + PAState close operations require PA lifecycle = Stopped.
  # Ensure that first so we don't run close-forwarder.ts after a half-failed PA close.
  if pa_state_account_exists; then
    echo "Ensuring PA is stopped before close-pdas..."
    confirm "Run emergency_stop on PA first?
close-pdas requires the PA to be in Stopped lifecycle. emergency_stop is
irreversible — the only way back to Running is a program upgrade."
    stop_pa
    echo ""
  fi

  confirm "Close ALL PA and forwarder PDA accounts on MAINNET.
This recovers rent SOL but is irreversible — you will need to re-initialize
PA state and forwarder config to use these programs again.

Order: PA markers → PA state → forwarder bitmaps → forwarder escrow → forwarder config"

  echo ""
  echo "=== Closing PA PDAs ==="
  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_WALLET="$MAINNET_WALLET" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/close-pdas.ts"

  echo ""
  echo "=== Closing Forwarder PDAs ==="
  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_WALLET="$MAINNET_WALLET" \
  TOKEN_MINT="EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/close-forwarder.ts"
}

cmd_stop() {
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  local pid
  pid="$(get_program_id "solana_pa_prototype")"
  if ! is_deployed "$pid"; then
    echo "❌ PA (${pid}) is not deployed on mainnet"
    exit 1
  fi

  confirm "Call emergency_stop on the PA on MAINNET.
This is IRREVERSIBLE — the only way back to Running is a program upgrade.
Stopped state: rejects settle, txdata_init, txdata_write, etc.
Required before close-pdas."

  stop_pa
}

cmd_full_reset() {
  local token_mint="${1:-}"
  if [[ -z "$token_mint" ]]; then
    echo "❌ TOKEN_MINT argument is required"
    echo "Usage: mainnet.sh full-reset <token_mint>"
    echo "Mainnet USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    exit 1
  fi

  require_cmd anchor
  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  local pa_pid
  pa_pid="$(get_program_id "solana_pa_prototype")"
  if ! is_deployed "$pa_pid"; then
    echo "❌ PA (${pa_pid}) is not deployed on mainnet"
    exit 1
  fi

  local fwd_pid
  fwd_pid="$(get_program_id "spl_token_forwarder")"
  if ! is_deployed "$fwd_pid"; then
    echo "❌ Forwarder (${fwd_pid}) is not deployed on mainnet"
    exit 1
  fi

  confirm "Full reset of mainnet PA + forwarder state.
Token mint: ${token_mint}

Will run, with no further prompts:
  1. emergency_stop on PA (irreversible — pauses settlement)
  2. close all PA markers + PAState
  3. close all forwarder nonce bitmaps + escrow ATA + config
  4. init a fresh PA state (new empty commitment tree)
  5. init forwarder config + escrow ATA for ${token_mint}

After completion the PA + forwarder are usable again with a clean slate.
All historical shielded resources are orphaned — their commitments are no
longer in the PA's Merkle tree."

  echo ""
  echo "=== 1/5  emergency_stop ==="
  MAINNET_AUTO_CONFIRM=1 stop_pa
  echo ""
  echo "=== 2/5 + 3/5  close-pdas (PA + forwarder) ==="
  MAINNET_AUTO_CONFIRM=1 cmd_close_pdas
  echo ""
  echo "=== 4/5  init PA ==="
  MAINNET_AUTO_CONFIRM=1 init_pa
  echo ""
  echo "=== 5/5  init forwarder for ${token_mint} ==="
  MAINNET_AUTO_CONFIRM=1 init_forwarder "$token_mint"
  echo ""
  echo "✅ Full reset complete"
}

cmd_close_expired_txdata() {
  local args=("$@")
  if (( ${#args[@]} > 1 )); then
    echo "❌ Too many close-expired-txdata arguments: ${args[*]}"
    echo "Usage: mainnet.sh close-expired-txdata [--dry-run]"
    exit 1
  fi

  local dry_run="${args[0]:-}"
  if [[ -n "$dry_run" && "$dry_run" != "--dry-run" ]]; then
    echo "❌ Unknown close-expired-txdata argument: ${dry_run}"
    echo "Usage: mainnet.sh close-expired-txdata [--dry-run]"
    exit 1
  fi

  require_cmd npx

  cd "$PROJECT_DIR"

  require_mainnet_wallet

  if [[ "$dry_run" == "--dry-run" ]]; then
    ANCHOR_PROVIDER_URL="$MAINNET_URL" \
    ANCHOR_WALLET="$MAINNET_WALLET" \
      npx ts-node -P tsconfig.json "${SCRIPT_DIR}/close-expired-txdata.ts" --dry-run
    return
  fi

  ANCHOR_PROVIDER_URL="$MAINNET_URL" \
  ANCHOR_WALLET="$MAINNET_WALLET" \
    npx ts-node -P tsconfig.json "${SCRIPT_DIR}/close-expired-txdata.ts"
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
  reset-forwarder-config)
    cmd_reset_forwarder_config "${2:-}"
    ;;
  recover-forwarder-escrow)
    cmd_recover_forwarder_escrow "${2:-}" "${3:-}"
    ;;
  balance)
    cmd_balance
    ;;
  close-pdas)
    cmd_close_pdas
    ;;
  close-expired-txdata)
    cmd_close_expired_txdata "${@:2}"
    ;;
  stop)
    cmd_stop
    ;;
  full-reset)
    cmd_full_reset "${2:-}"
    ;;
  *)
    echo "Usage: mainnet.sh <command> [target]"
    echo ""
    echo "Commands:"
    echo "  deploy [pa|anomapay-forwarder|all]        First-time deploy to mainnet (default: all)"
    echo "  upgrade [pa|anomapay-forwarder|all]       Rebuild + deploy over existing programs"
    echo "  init                            Initialize PA state (idempotent)"
    echo "  init-forwarder <mint>           Initialize forwarder + escrow for token mint"
    echo "  reset-forwarder-config <mint>   Close/re-init only the forwarder config PDA"
    echo "  recover-forwarder-escrow <mint> <recipient_owner>"
    echo "                                  Drain one escrow, then recreate it empty"
    echo "  close-pdas                      Close all PA + forwarder PDAs, recover rent"
    echo "  close-expired-txdata [--dry-run] Close expired TxData accounts only"
    echo "  stop                            emergency_stop the PA (irreversible)"
    echo "  full-reset <mint>               stop → close all PDAs → init PA → init forwarder"
    echo "  status                          Show deployment status + wallet balance"
    echo "  balance                         Show wallet address and balance"
    echo ""
    echo "Mainnet USDC: EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
    echo ""
    echo "Environment variables:"
    echo "  MAINNET_RPC_URL         RPC endpoint (default: https://api.mainnet-beta.solana.com)"
    echo "  MAINNET_WALLET          Keypair file (default: scripts/mainnet-wallet.json)"
    echo "  MAINNET_AUTO_CONFIRM=1  Skip all interactive 'Type yes to proceed' prompts"
    exit 1
    ;;
esac
