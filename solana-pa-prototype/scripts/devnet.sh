#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

DEVNET_URL="${DEVNET_URL:-https://api.devnet.solana.com}"
DEVNET_WALLET="${PROJECT_DIR}/scripts/devnet-wallet.json"

# Program registry — add new programs here.
# Keys are shorthand names, values are the underscore-delimited binary names
# matching target/deploy/${value}-keypair.json and target/deploy/${value}.so.
declare -A PROGRAMS=(
  [pa]="solana_pa_prototype"
  [btf]="block_time_forwarder"
)

# Programs that need post-deploy initialization
PA_NEEDS_INIT=true

# Package names build_programs() below builds by name. A program added under
# programs/ that isn't in this list would otherwise silently stop being built
# on a forward-merge — fail loudly instead. Keep in sync with dev.sh and
# validator-deploy.sh's equivalent lists.
EXPECTED_PROGRAMS=(solana-pa-prototype block-time-forwarder test-forwarder)

assert_known_programs() {
  local dir pkg known ok
  for dir in "$PROJECT_DIR"/programs/*/; do
    pkg="$(sed -n 's/^name = "\(.*\)"$/\1/p' "${dir}Cargo.toml" | head -1)"
    ok=0
    for known in "${EXPECTED_PROGRAMS[@]}"; do
      if [[ "$pkg" == "$known" ]]; then
        ok=1
        break
      fi
    done
    if [[ $ok -eq 0 ]]; then
      echo "❌ Unrecognized program under programs/: '${pkg}' (${dir})" >&2
      echo "   devnet.sh, dev.sh, and validator-deploy.sh build/deploy programs by" >&2
      echo "   name. Add '${pkg}' to EXPECTED_PROGRAMS and to the build/deploy" >&2
      echo "   commands in all three scripts before proceeding — otherwise it" >&2
      echo "   silently never gets built or deployed." >&2
      exit 1
    fi
  done
}

# ---------- helpers ----------

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "❌ Missing required command: $1"
    echo "Run this inside the Nix dev shell: nix develop"
    exit 1
  fi
}

ensure_devnet_wallet() {
  if [[ ! -f "$DEVNET_WALLET" ]]; then
    echo "Generating devnet wallet at ${DEVNET_WALLET}..."
    solana-keygen new --no-bip39-passphrase -o "$DEVNET_WALLET"
  fi
  local pubkey
  pubkey="$(solana-keygen pubkey "$DEVNET_WALLET")"
  echo "Devnet wallet: ${pubkey}"
}

require_devnet_wallet() {
  if [[ ! -f "$DEVNET_WALLET" ]]; then
    echo "❌ No devnet wallet found at ${DEVNET_WALLET}"
    echo "Run: ./scripts/dev.sh devnet deploy"
    exit 1
  fi
}

get_wallet_pubkey() {
  solana-keygen pubkey "$DEVNET_WALLET"
}

get_balance() {
  # Returns numeric SOL balance (e.g. "3.5")
  solana balance --keypair "$DEVNET_WALLET" --url "$DEVNET_URL" | awk '{print $1}'
}

ensure_balance() {
  local min_sol="$1"
  local balance
  balance="$(get_balance)"

  if awk "BEGIN{exit ($balance >= $min_sol) ? 0 : 1}"; then
    echo "Balance: ${balance} SOL (need ${min_sol})"
    return 0
  fi

  echo "❌ Insufficient balance: ${balance} SOL (need ${min_sol})"
  echo "Wallet: $(get_wallet_pubkey)"
  echo "Transfer SOL to this wallet before proceeding."
  exit 1
}

get_program_id() {
  local name="$1"
  solana-keygen pubkey "target/deploy/${name}-keypair.json"
}

is_deployed() {
  local program_id="$1"
  solana program show "$program_id" --url "$DEVNET_URL" >/dev/null 2>&1
}

deploy_program() {
  local name="$1"
  local program_id
  program_id="$(get_program_id "$name")"

  echo "Deploying ${name} (${program_id})..."
  if ! solana program deploy \
    "target/deploy/${name}.so" \
    --keypair "$DEVNET_WALLET" \
    --program-id "target/deploy/${name}-keypair.json" \
    --url "$DEVNET_URL" 2>&1; then
    echo ""
    echo "❌ Deploy failed for ${name}."
    echo "If the program was previously closed (teardown), the ID is permanently burned."
    echo "To recover: delete target/deploy/${name}-keypair.json, run 'anchor build --no-idl'"
    echo "to generate a new keypair, then deploy again."
    exit 1
  fi
  echo "  ✅ ${name} deployed: ${program_id}"
  echo "  https://explorer.solana.com/address/${program_id}?cluster=devnet"
}

close_program() {
  local name="$1"
  local program_id
  program_id="$(get_program_id "$name")"

  if ! is_deployed "$program_id"; then
    echo "  ${name} (${program_id}): not deployed, skipping"
    return 0
  fi

  echo "Closing ${name} (${program_id})..."
  solana program close "$program_id" \
    --keypair "$DEVNET_WALLET" \
    --url "$DEVNET_URL" \
    --bypass-warning
  echo "  ✅ ${name} closed, rent reclaimed"
}

build_programs() {
  assert_known_programs
  echo "Building programs..."
  # Devnet is a development network, and this script's teardown/close-pdas
  # commands exist to reset it — so devnet builds carry dev-teardown, which
  # enables close_markers_batch (marker PDA reclamation). Without it,
  # cmd_teardown/cmd_close_pdas cannot reclaim marker rent at all.
  # dev-teardown is a solana-pa-prototype-only Cargo feature, so it must be
  # scoped with -p rather than passed to the whole-workspace build.
  anchor build -p solana-pa-prototype --no-idl -- --features dev-teardown
  anchor build -p block-time-forwarder --no-idl
  anchor build -p test-forwarder --no-idl
}

init_pa() {
  echo "Initializing PA (idempotent)..."
  local program_id
  program_id="$(get_program_id "solana_pa_prototype")"

  ANCHOR_PROVIDER_URL="$DEVNET_URL" \
  ANCHOR_WALLET="$DEVNET_WALLET" \
    npx ts-node "${SCRIPT_DIR}/devnet-init-pa.ts"
}

# Resolve target list from user argument.
# Returns space-separated shorthand names (e.g. "pa btf").
resolve_targets() {
  local target="${1:-all}"
  case "$target" in
    all)
      echo "${!PROGRAMS[*]}"
      ;;
    pa|btf)
      echo "$target"
      ;;
    *)
      echo "❌ Unknown target: ${target}" >&2
      echo "Valid targets: pa, btf, all" >&2
      exit 1
      ;;
  esac
}

# Estimate minimum SOL needed for deployment.
# PA ~4.7 SOL (659K binary), BTF ~1.3 SOL (177K binary).
# Estimates include headroom for transaction fees.
estimate_balance_needed() {
  local targets="$1"
  local total=0
  for t in $targets; do
    case "$t" in
      pa)  total=$(awk "BEGIN{print $total + 5}") ;;
      btf) total=$(awk "BEGIN{print $total + 2}") ;;
    esac
  done
  echo "$total"
}

print_explorer_link() {
  local program_id="$1"
  echo "  https://explorer.solana.com/address/${program_id}?cluster=devnet"
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

  ensure_devnet_wallet
  build_programs
  ensure_balance "$min_sol"

  for t in $targets; do
    deploy_program "${PROGRAMS[$t]}"
  done

  # Initialize PA if it was deployed
  if [[ "$PA_NEEDS_INIT" == "true" ]] && [[ " $targets " == *" pa "* ]]; then
    init_pa
  fi

  echo ""
  echo "✅ Deploy complete"
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    echo "  ${t}: ${pid}"
  done
}

cmd_close_pdas() {
  local flag="${1:-}"

  require_cmd npx

  cd "$PROJECT_DIR"

  require_devnet_wallet

  echo "Closing PA PDA accounts..."
  ANCHOR_PROVIDER_URL="$DEVNET_URL" \
  ANCHOR_WALLET="$DEVNET_WALLET" \
  npx ts-node -P tsconfig.json scripts/close-pdas.ts ${flag:+"$flag"}
}

cmd_teardown() {
  local targets
  targets="$(resolve_targets "${1:-all}")"

  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_devnet_wallet

  echo "⚠️  WARNING: solana program close is PERMANENT."
  echo "Closed program IDs cannot be reused. You will need new keypairs to deploy again."
  echo ""

  # Close PDA accounts first (programs must still be deployed for close instructions to work)
  echo "==> Closing PDA accounts before closing programs..."
  cmd_close_pdas || echo "⚠ PDA close failed or partially completed — continuing with program close"
  echo ""

  for t in $targets; do
    close_program "${PROGRAMS[$t]}"
  done

  echo ""
  echo "✅ Teardown complete"
}

cmd_upgrade() {
  local targets
  targets="$(resolve_targets "${1:-all}")"

  require_cmd anchor
  require_cmd solana
  require_cmd solana-keygen
  require_cmd npx

  cd "$PROJECT_DIR"

  require_devnet_wallet

  # Verify target programs are already deployed
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    if ! is_deployed "$pid"; then
      echo "❌ ${t} (${pid}) is not deployed — use 'deploy' for first-time deployment"
      exit 1
    fi
  done

  build_programs

  # Deploy overwrites the existing program binary in-place (no close needed)
  for t in $targets; do
    deploy_program "${PROGRAMS[$t]}"
  done

  echo ""
  echo "✅ Upgrade complete"
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    echo "  ${t}: ${pid}"
  done
}

cmd_test() {
  require_cmd solana
  require_cmd solana-keygen
  require_cmd yarn
  require_cmd node

  cd "$PROJECT_DIR"

  require_devnet_wallet
  ensure_balance 2

  # Verify both programs are deployed
  for t in "${!PROGRAMS[@]}"; do
    local pid
    pid="$(get_program_id "${PROGRAMS[$t]}")"
    if ! is_deployed "$pid"; then
      echo "❌ ${t} (${pid}) is not deployed on devnet"
      echo "Run: ./scripts/dev.sh devnet deploy"
      exit 1
    fi
  done

  # Ensure node_modules are present
  if ! yarn install --frozen-lockfile 2>/dev/null; then
    echo "Lockfile out of sync, regenerating..."
    rm -f yarn.lock package-lock.json
    yarn install
  fi

  # Devnet-safe test describe blocks (explicit allowlist).
  # Tests that require test-forwarder or permanently mutate state are excluded.
  local grep_pattern
  grep_pattern=$(cat <<'GREP'
Groth16 batch aggregation E2E|Re-initialization guard|Direct settle & duplicate nullifier|Settle error paths|Issue #6: Emergency Stop|TxData Expiration|TxData authority and bounds checks|update_expiry_config|TxData expiration enforcement|Settlement error paths — fixture variants|Tree growth and multi-settlement
GREP
  )

  echo "Running devnet-safe integration tests..."
  ANCHOR_PROVIDER_URL="$DEVNET_URL" \
  ANCHOR_WALLET="$DEVNET_WALLET" \
    yarn run ts-mocha -p ./tsconfig.json -t 1000000 \
      --grep "$grep_pattern" \
      'tests/**/*.ts'

  echo ""
  echo "✅ Devnet tests passed"
}

cmd_status() {
  require_cmd solana
  require_cmd solana-keygen

  cd "$PROJECT_DIR"

  echo "=== Devnet Status ==="
  echo ""

  # Wallet
  if [[ -f "$DEVNET_WALLET" ]]; then
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
    # Derive PAState PDA: seeds = ["pa_state"], program = PA
    local pa_state
    pa_state="$(solana find-program-derived-address "$pa_pid" string:pa_state --url "$DEVNET_URL" 2>/dev/null | head -1 || true)"
    if [[ -n "$pa_state" ]]; then
      local pa_state_addr
      pa_state_addr="$(echo "$pa_state" | awk '{print $1}')"
      if solana account "$pa_state_addr" --url "$DEVNET_URL" >/dev/null 2>&1; then
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

  require_devnet_wallet

  local pid
  pid="$(get_program_id "solana_pa_prototype")"
  if ! is_deployed "$pid"; then
    echo "❌ PA (${pid}) is not deployed on devnet"
    echo "Run: ./scripts/dev.sh devnet deploy pa"
    exit 1
  fi

  init_pa
}

cmd_balance() {
  require_cmd solana
  require_cmd solana-keygen

  if [[ ! -f "$DEVNET_WALLET" ]]; then
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
  teardown)
    cmd_teardown "${2:-all}"
    ;;
  upgrade)
    cmd_upgrade "${2:-all}"
    ;;
  test)
    cmd_test
    ;;
  status)
    cmd_status
    ;;
  init)
    cmd_init
    ;;
  balance)
    cmd_balance
    ;;
  close-pdas)
    cmd_close_pdas "${2:-}"
    ;;
  close-pa-state)
    cmd_close_pdas "--pa-state-only"
    ;;
  *)
    echo "Usage: devnet.sh <command> [target]"
    echo ""
    echo "Commands:"
    echo "  deploy [pa|btf|all]      First-time deploy to devnet (default: all)"
    echo "  upgrade [pa|btf|all]     Rebuild + deploy over existing programs"
    echo "  teardown [pa|btf|all]    PERMANENT: close programs, reclaim rent"
    echo "  close-pdas               Close all PA PDA accounts, reclaim rent"
    echo "  close-pa-state           Close only PAState (for re-init after upgrade)"
    echo "  test                     Run integration tests against devnet"
    echo "  init                     Initialize PA state (idempotent)"
    echo "  status                   Show deployment status + wallet balance"
    echo "  balance                  Show wallet address and balance"
    exit 1
    ;;
esac
