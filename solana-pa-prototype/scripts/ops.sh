#!/usr/bin/env bash
# Cluster operations for the PA programs: build, deploy, initialize, status,
# pause/unpause. One code path for every cluster — the target
# cluster is a flag, never baked into a script. All per-cluster differences
# (RPC URL, explorer links, wallet default, dev-teardown policy) are data set
# in resolve_cluster.
#
# Run through ./scripts/dev.sh, which enters the Nix shell:
#   ./scripts/dev.sh deploy pa --cluster devnet
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# shellcheck source=validator-deploy.sh
source "${SCRIPT_DIR}/validator-deploy.sh"

usage() {
  cat <<USAGE
Usage: ops.sh <command> [target] --cluster <localnet|devnet|mainnet> [flags]

Commands:
  deploy [${DEPLOY_TARGETS}|all]
                         First-time deploy (default target: all). Deploys the
                         production build; on localnet the PA and the SPL
                         token forwarder are initialized at once, while on
                         devnet/mainnet each program's IDL is published and
                         init / forwarder init are left for after the
                         metadata accounts are given to the owner.
  upgrade [${DEPLOY_TARGETS}|all]
                         Rebuild + upgrade existing programs in place: through
                         their own upgrade (owner wallet) once their upgrade
                         authority is their PDA, else through the loader
  init                   Initialize PA state (idempotent)
  set-kind-table         Replace the PA's kind-table commitment with
                         PA_KIND_TABLE_COMMITMENT (owner wallet).
  deny-logic-ref         Deny PA_DENIED_LOGIC_REF: no settlement consumes or
                         creates a resource carrying it again. Cannot be
                         undone (owner wallet).
  forwarder <cmd>        SPL token forwarder operations: init, reinitialize,
                         emergency-withdraw. Parameters are STF_*
                         environment variables; see scripts/forwarder.ts.
  lookup-table           Create the deployment's settlement lookup table, or
                         extend the one in PA_LOOKUP_TABLE with any missing
                         key; STF_TOKEN_MINTS adds mints' escrow accounts.
                         See scripts/lookup-table.ts.
  pause                  Pause settlement (owner-only; pa-evm's pause())
  unpause                Resume settlement (owner-only; pa-evm's unpause())
  status                 Show deployment status + wallet balance
  balance                Show wallet address and balance
  idl-publish [${DEPLOY_TARGETS}|all]
                         Publish each program's production IDL on chain (the
                         program's canonical Program Metadata IDL account;
                         devnet/mainnet only; signer must be the upgrade
                         authority to create it, its authority to update it)
  test [--cluster <c>] [spec file...]
                         No cluster (or localnet): full deterministic local
                         integration flow, every spec file on one validator.
                         devnet/mainnet: proves a fixture set for the
                         deployment (new salt, PA_KIND_TABLE), then runs the
                         spec files that build on any state, without the
                         tests tagged @localnet, against the programs
                         deployed there; refused when the wallet holds an
                         owner's role (a stored owner, or an upgrade
                         authority that is not the program's PDA). Spec files
                         (paths under tests/) restrict the run to them.
  build-dev [--no-idl]   Build all programs (dev-teardown enabled), no deploy
  build-release          Build the production binaries, no deploy (verifies
                         each production IDL is its development IDL minus
                         the declared dev-only instructions)
  clippy                 Lint every program, and each one with dev features
                         again with them enabled
  sync-ids               Sync declare_id!/Anchor.toml to the committed
                         program keypairs (use after rotating IDs)
  verify-build [--cluster <c>]
                         Deterministic solana-verify Docker build of the PA;
                         with a cluster, compares against the deployed hash
  validator              Start the local test validator (RISC0 verifier stack
                         copied from devnet, marker fixtures preloaded)
  refresh-devnet-verifier --url <rpc>
                         Replace the committed copy of the devnet RISC0
                         verifier stack (devnet-verifier/) with devnet's
                         current state
  validator-deploy       Sync IDs, build, start the validator with all
                         programs loaded at genesis, and keep it running

Flags:
  --cluster <c>    Target cluster (required except test/build-dev/build-release/
                   clippy/sync-ids/validator/validator-deploy; optional for
                   verify-build)
  --wallet <path>  Wallet keypair. Defaults: devnet → scripts/devnet-wallet.json,
                   localnet → ~/.config/solana/id.json, mainnet → none (required).
                   The wallet must exist; nothing is auto-generated.
  --url <rpc>      The cluster's RPC endpoint. devnet and mainnet have no
                   default: pass --url or set DEVNET_RPC_URL / MAINNET_RPC_URL
                   (the operator's RPC provider, never the public endpoint)
  --no-idl         build-dev: skip IDL generation (faster compile check)
  --dev-teardown   deploy/upgrade: build with the dev-teardown feature
                   (close_markers_batch enabled). Localnet only.
  --prebuilt       deploy/upgrade: ship the existing target/deploy artifacts
                   without rebuilding (for verify-build output); test: run the
                   cluster run against the programs already deployed (a
                   running local validator included)
  --mode <m>       test: real runs the suite against Groth16 fixtures and
                   the devnet-cloned verifier; mock runs it against mock
                   fixtures and the localnet mock verifier. mock is
                   localnet-only. Default: PA_TEST_MODE, else real.

Initialization parameters (required by deploy/init when the PA is a target):
  PA_OWNER             The adapter's initial owner (base58), who alone pauses,
                       upgrades and configures it, as pa-evm's initialOwner.
  PA_VERIFIER_ROUTER   RISC0 verifier router program ID (base58).
                       Devnet: ${VERIFIER_ROUTER}
  PA_PROOF_SELECTOR    4-byte Groth16 verifier selector (hex).
                       Devnet: ${GROTH16_SELECTOR}

The PA starts on the empty kind table; set-kind-table installs another.

Forwarder initialization parameters (required by deploy/forwarder init when
the SPL token forwarder is a target):
  STF_LOGIC_REF        32-byte hex logic ref the forwarder serves
  STF_EMERGENCY_COMMITTEE
                       base58 pubkey of the emergency committee
  STF_OWNER            base58 pubkey of the forwarder's initial owner, who
                       alone upgrades it and rotates its logic ref
  STF_TOKEN_MINT       optional: base58 mint whose escrow ATA to create
USAGE
  exit 1
}

# ---------- argument parsing ----------

COMMAND=""
TARGET=""
CLUSTER=""
WALLET_OVERRIDE=""
RPC_OVERRIDE=""
NO_IDL=false
DEV_TEARDOWN=false
PREBUILT=false
TEST_MODE="${PA_TEST_MODE:-real}"
SPEC_FILES=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --cluster)
      [[ $# -ge 2 ]] || { echo "❌ --cluster requires a value" >&2; exit 1; }
      CLUSTER="$2"
      shift 2
      ;;
    --wallet)
      [[ $# -ge 2 ]] || { echo "❌ --wallet requires a value" >&2; exit 1; }
      WALLET_OVERRIDE="$2"
      shift 2
      ;;
    --url)
      [[ $# -ge 2 ]] || { echo "❌ --url requires a value" >&2; exit 1; }
      RPC_OVERRIDE="$2"
      shift 2
      ;;
    --no-idl)
      NO_IDL=true
      shift
      ;;
    --dev-teardown)
      DEV_TEARDOWN=true
      shift
      ;;
    --prebuilt)
      PREBUILT=true
      shift
      ;;
    --mode)
      [[ $# -ge 2 ]] || { echo "❌ --mode requires a value" >&2; exit 1; }
      TEST_MODE="$2"
      shift 2
      ;;
    --*)
      echo "❌ Unknown flag: $1" >&2
      usage
      ;;
    *)
      if [[ -z "$COMMAND" ]]; then
        COMMAND="$1"
      elif [[ "$COMMAND" == "test" ]]; then
        SPEC_FILES+=("$1")
      elif [[ -z "$TARGET" ]]; then
        TARGET="$1"
      else
        echo "❌ Unexpected argument: $1" >&2
        usage
      fi
      shift
      ;;
  esac
done

validate_test_mode "$TEST_MODE"
[[ -n "$COMMAND" ]] || usage

# ---------- cluster resolution ----------

RPC_URL=""
EXPLORER_QS=""
PRINT_EXPLORER=false
ALLOW_DEV_TEARDOWN=false
WALLET=""

resolve_cluster() {
  local default_wallet=""
  case "$CLUSTER" in
    localnet)
      # validator-deploy.sh's defaults for the local test validator
      RPC_URL="$CLUSTER_URL"
      default_wallet="$ANCHOR_WALLET_PATH"
      ALLOW_DEV_TEARDOWN=true
      ;;
    devnet)
      RPC_URL="${DEVNET_RPC_URL:-}"
      EXPLORER_QS="?cluster=devnet"
      PRINT_EXPLORER=true
      default_wallet="${PROJECT_DIR}/scripts/devnet-wallet.json"
      ;;
    mainnet)
      RPC_URL="${MAINNET_RPC_URL:-}"
      PRINT_EXPLORER=true
      ;;
    "")
      echo "❌ Missing --cluster <localnet|devnet|mainnet>" >&2
      exit 1
      ;;
    *)
      echo "❌ Unknown cluster: ${CLUSTER} (expected localnet, devnet, or mainnet)" >&2
      exit 1
      ;;
  esac

  if [[ -n "$RPC_OVERRIDE" ]]; then
    RPC_URL="$RPC_OVERRIDE"
  fi
  # A cluster operation goes through the operator's RPC provider, never the
  # rate-limited public endpoint.
  if [[ -z "$RPC_URL" ]]; then
    echo "❌ No RPC endpoint for ${CLUSTER}: pass --url <rpc> or set ${CLUSTER^^}_RPC_URL." >&2
    exit 1
  fi

  WALLET="${WALLET_OVERRIDE:-$default_wallet}"
  if [[ -z "$WALLET" ]]; then
    echo "❌ No default wallet for cluster '${CLUSTER}' — pass --wallet <path>" >&2
    exit 1
  fi
  if [[ ! -f "$WALLET" ]]; then
    echo "❌ Wallet not found: ${WALLET}" >&2
    echo "   Nothing is auto-generated. Provide an existing, funded keypair" >&2
    echo "   (or pass a different one with --wallet)." >&2
    exit 1
  fi
}

# ---------- helpers ----------

get_wallet_pubkey() {
  solana-keygen pubkey "$WALLET"
}

get_balance() {
  # Returns numeric SOL balance (e.g. "3.5")
  solana balance --keypair "$WALLET" --url "$RPC_URL" | awk '{print $1}'
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

# True when <program_id> is a program on the cluster, false when no account
# exists there; any other failure (an unreachable RPC) exits.
is_deployed() {
  local program_id="$1" out
  if out="$(solana program show "$program_id" --url "$RPC_URL" 2>&1)"; then
    return 0
  fi
  if [[ "$out" == "Error: Unable to find the account ${program_id}" ]]; then
    return 1
  fi
  echo "❌ solana program show ${program_id} failed: ${out}" >&2
  exit 1
}

# Exit unless program <pid> (called <label> in the message) is deployed on
# the cluster; with <deploy-args>, name the dev.sh command that deploys it.
require_deployed() {
  local pid="$1" label="$2" deploy_args="${3:-}"
  if ! is_deployed "$pid"; then
    echo "❌ ${label} (${pid}) is not deployed on ${CLUSTER}"
    if [[ -n "$deploy_args" ]]; then
      echo "Run: ./scripts/dev.sh ${deploy_args} --cluster ${CLUSTER}"
    fi
    exit 1
  fi
}

print_explorer_link() {
  local address="$1"
  if [[ "$PRINT_EXPLORER" == "true" ]]; then
    echo "  https://explorer.solana.com/address/${address}${EXPLORER_QS}"
  fi
}

# An upgrade writes the new binary over the existing program-data account,
# which must be at least as large. `solana program deploy` extends it by the
# exact shortfall, but the upgradeable loader rejects ExtendProgram below
# 10240 bytes ("ExtendProgram requires a minimum of 10240 additional bytes or
# to extend to maximum size"), so a small growth fails the deploy. Extend by
# the shortfall, raised to that minimum, before deploying.
extend_program_data_if_needed() {
  local name="$1"
  local program_id="$2"
  is_deployed "$program_id" || return 0
  local current_len new_len shortfall
  current_len="$(solana program show "$program_id" --url "$RPC_URL" | awk '/^Data Length:/ {print $3}')"
  [[ "$current_len" =~ ^[0-9]+$ ]] || { echo "❌ Could not read the data length of ${program_id}" >&2; exit 1; }
  new_len="$(stat -c %s "target/deploy/${name}.so")"
  (( new_len > current_len )) || return 0
  shortfall=$(( new_len - current_len ))
  local min_extend=10240
  (( shortfall < min_extend )) && shortfall=$min_extend
  echo "Extending ${name} program data by ${shortfall} bytes (${current_len} on chain, ${new_len} needed)..."
  solana program extend "$program_id" "$shortfall" --keypair "$WALLET" --url "$RPC_URL"
}

deploy_one() {
  local name="$1"
  local program_id
  program_id="$(get_program_id "$name")"

  extend_program_data_if_needed "$name" "$program_id"
  echo "Deploying ${name} (${program_id})..."
  if ! solana program deploy \
    "target/deploy/${name}.so" \
    --keypair "$WALLET" \
    --program-id "target/deploy/${name}-keypair.json" \
    --url "$RPC_URL" 2>&1; then
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

# The programs that own their upgrades once initialized: their upgrade
# authority becomes their own PDA, and only their owner-only `upgrade`
# replaces their code, as pa-evm's UUPS contracts authorize their own
# upgrades.
SELF_UPGRADING_PROGRAMS=(protocol_adapter spl_token_forwarder)

# Upgrade <name> in place along the path its upgrade authority leaves: through
# the program when the authority is the program's own PDA, through the
# loader while the wallet still holds it (before `initialize` or
# `migrate_state` hands it over), and through the loader for a program that
# never owns its upgrades.
upgrade_one() {
  local name="$1" path
  if [[ " ${SELF_UPGRADING_PROGRAMS[*]} " != *" ${name} "* ]]; then
    deploy_one "$name"
    return
  fi
  path="$(run_ts scripts/upgrade-program.ts path "$name")"
  case "$path" in
    loader) deploy_one "$name" ;;
    program) upgrade_through_program "$name" ;;
    *)
      echo "❌ upgrade-program.ts printed '${path}' as ${name}'s upgrade path" >&2
      exit 1
      ;;
  esac
}

# The owner writes the new code to a loader buffer and calls the program's
# `upgrade` with it, which hands the buffer to the program's upgrade
# authority PDA and upgrades.
upgrade_through_program() {
  local name="$1" program_id output buffer
  program_id="$(get_program_id "$name")"
  extend_program_data_if_needed "$name" "$program_id"
  echo "Writing ${name}'s code to a buffer..."
  output="$(solana program write-buffer "target/deploy/${name}.so" --keypair "$WALLET" --url "$RPC_URL")"
  echo "$output"
  buffer="$(awk '/^Buffer:/ {print $2}' <<<"$output")"
  if [[ -z "$buffer" ]]; then
    echo "❌ solana program write-buffer printed no buffer address" >&2
    exit 1
  fi
  if ! run_ts scripts/upgrade-program.ts upgrade "$name" "$buffer"; then
    echo "❌ The upgrade of ${name} failed. Buffer ${buffer} still holds its rent, under the wallet's authority:" >&2
    echo "   reclaim it with: solana program close ${buffer} --keypair ${WALLET} --url <rpc>" >&2
    exit 1
  fi
  echo "  ✅ ${name} upgraded: ${program_id}"
  print_explorer_link "$program_id"
}

build_for_deploy() {
  if [[ "$PREBUILT" == "true" ]]; then
    if [[ "$DEV_TEARDOWN" == "true" ]]; then
      echo "❌ --prebuilt and --dev-teardown are contradictory: --prebuilt deploys" >&2
      echo "   existing artifacts without building anything." >&2
      exit 1
    fi
    # Deploy the artifacts already in target/deploy/ — the path for shipping
    # a solana-verify deterministic build, which a rebuild here would clobber.
    local t so
    for t in $(resolve_targets "$TARGET"); do
      so="target/deploy/${PROGRAM_BY_TARGET[$t]}.so"
      if [[ ! -f "$so" ]]; then
        echo "❌ --prebuilt: ${so} does not exist. Build it first (e.g. verify-build" >&2
        echo "   for the PA's deterministic artifact, build-release for the rest)." >&2
        exit 1
      fi
    done
    echo "    Deploying prebuilt artifacts from target/deploy/ (no build)."
    return 0
  fi

  if [[ "$DEV_TEARDOWN" == "true" ]]; then
    if [[ "$ALLOW_DEV_TEARDOWN" != "true" ]]; then
      echo "❌ --dev-teardown is refused on ${CLUSTER}: close_markers_batch deletes" >&2
      echo "   nullifier markers (replay protection) and never exists on a live" >&2
      echo "   cluster." >&2
      exit 1
    fi
    build_programs_dev
  else
    build_programs_release
  fi
}

# Cluster deploys never generate fresh program IDs implicitly: an ID that
# isn't committed (or already present locally) would deploy to an address
# nothing else knows about.
require_deploy_keypairs() {
  if ! restore_program_keypairs; then
    echo "❌ Program keypairs are missing from target/deploy/ and not in git." >&2
    echo "   Generate them with './scripts/dev.sh anchor-build', commit the ones" >&2
    echo "   you intend to deploy, then re-run." >&2
    exit 1
  fi
}

# The deployed binary bakes in declare_id!, and every PDA derivation depends
# on it. If declare_id! and the deploy keypair disagree, the deployment is
# broken in ways that only surface at settlement time — catch it here.
assert_declare_id_synced() {
  local targets="$1"
  local t name lib_rs declared actual
  load_workspace_programs
  for t in $targets; do
    name="${PROGRAM_BY_TARGET[$t]}"
    lib_rs="${PROGRAM_SRC[$name]}"
    declared="$(read_declare_id "$lib_rs")"
    actual="$(get_program_id "$name")"
    if [[ "$declared" != "$actual" ]]; then
      echo "❌ ${name}: declare_id! (${declared}) does not match the deploy keypair (${actual})." >&2
      echo "   Sync them before deploying (anchor-test runs sync_program_ids, or fix ${lib_rs})." >&2
      exit 1
    fi
  done
}

require_init_params() {
  if [[ -z "${PA_OWNER:-}" || -z "${PA_VERIFIER_ROUTER:-}" || -z "${PA_PROOF_SELECTOR:-}" ]]; then
    echo "❌ Missing PA_OWNER, PA_VERIFIER_ROUTER and/or PA_PROOF_SELECTOR." >&2
    echo "   initialize sets the owner and pins the verifier router and proof" >&2
    echo "   selector for the lifetime of the deployment; there is no safe default." >&2
    echo "   Devnet values:" >&2
    echo "     PA_VERIFIER_ROUTER=${VERIFIER_ROUTER}" >&2
    echo "     PA_PROOF_SELECTOR=${GROTH16_SELECTOR}" >&2
    exit 1
  fi
}

# Resolve target list from user argument to space-separated target names.
resolve_targets() {
  local target="${1:-all}"
  if [[ "$target" == "all" ]]; then
    echo "${PROGRAM_TARGETS[*]}"
  elif [[ -n "${PROGRAM_BY_TARGET[$target]+set}" ]]; then
    echo "$target"
  else
    echo "❌ Unknown target: ${target}" >&2
    echo "Valid targets: ${PROGRAM_TARGETS[*]}, all" >&2
    exit 1
  fi
}

# Minimum SOL needed to deploy the given targets (PROGRAM_TABLE's sol column).
estimate_balance_needed() {
  local targets="$1"
  local t total=0
  for t in $targets; do
    total=$(awk "BEGIN{print $total + ${PROGRAM_DEPLOY_SOL[$t]}}")
  done
  echo "$total"
}

run_ts() {
  local script="$1"
  shift
  ANCHOR_PROVIDER_URL="$RPC_URL" \
  ANCHOR_WALLET="$WALLET" \
    npx ts-node -P tsconfig.json "$script" "$@"
}

init_pa() {
  echo "Initializing PA (idempotent)..."
  run_ts scripts/init-pa.ts
}

require_forwarder_init_params() {
  if [[ -z "${STF_LOGIC_REF:-}" || -z "${STF_EMERGENCY_COMMITTEE:-}" || -z "${STF_OWNER:-}" ]]; then
    echo "❌ Missing STF_LOGIC_REF, STF_EMERGENCY_COMMITTEE and/or STF_OWNER." >&2
    echo "   The forwarder config pins the logic ref it serves, the committee" >&2
    echo "   that can act in an emergency and the owner; there is no safe default." >&2
    exit 1
  fi
}

init_forwarder() {
  echo "Initializing SPL token forwarder (idempotent)..."
  run_ts scripts/forwarder.ts init
}

# ---------- commands ----------

cmd_deploy() {
  local targets
  targets="$(resolve_targets "$TARGET")"

  require_cmd anchor
  require_cmd npx

  if [[ " $targets " == *" pa "* ]]; then
    require_init_params
  fi
  if [[ " $targets " == *" stf "* ]]; then
    require_forwarder_init_params
  fi

  require_deploy_keypairs
  assert_declare_id_synced "$targets"
  ensure_balance "$(estimate_balance_needed "$targets")"
  build_for_deploy

  for t in $targets; do
    deploy_one "${PROGRAM_BY_TARGET[$t]}"
  done

  # `initialize` hands each self-upgrading program's upgrade authority to the
  # program, and only the upgrade authority creates the program's canonical
  # metadata accounts. On a cluster with the Program Metadata program the IDLs
  # are published first, and initialization waits until the deployer has given
  # those accounts to the owner, by hand (docs/OPERATIONS.md).
  if [[ "$CLUSTER" == "localnet" ]]; then
    if [[ " $targets " == *" pa "* ]]; then
      init_pa
    fi
    if [[ " $targets " == *" stf "* ]]; then
      init_forwarder
    fi
  else
    cmd_idl_publish
    echo "Next, by hand: give each program's canonical metadata accounts to its owner, then run" \
      "init and forwarder init (docs/OPERATIONS.md, Deploy and initialize)."
  fi

  echo ""
  echo "✅ Deploy complete (${CLUSTER})"
  for t in $targets; do
    echo "  ${t}: $(get_program_id "${PROGRAM_BY_TARGET[$t]}")"
  done
}

cmd_upgrade() {
  local targets
  targets="$(resolve_targets "$TARGET")"

  require_cmd anchor

  require_deploy_keypairs
  assert_declare_id_synced "$targets"

  # Verify target programs are already deployed
  for t in $targets; do
    local pid
    pid="$(get_program_id "${PROGRAM_BY_TARGET[$t]}")"
    if ! is_deployed "$pid"; then
      echo "❌ ${t} (${pid}) is not deployed — use 'deploy' for first-time deployment"
      exit 1
    fi
  done

  build_for_deploy

  for t in $targets; do
    upgrade_one "${PROGRAM_BY_TARGET[$t]}"
  done

  echo ""
  echo "✅ Upgrade complete (${CLUSTER})"
  for t in $targets; do
    echo "  ${t}: $(get_program_id "${PROGRAM_BY_TARGET[$t]}")"
  done
}

# The adapter operations below run a TS script against the deployed PA.
require_pa_deployed() {
  require_cmd npx
  local pid
  pid="$(get_program_id "protocol_adapter")"
  require_deployed "$pid" "PA" "deploy pa"
}

cmd_init() {
  require_init_params
  require_pa_deployed
  init_pa
}

cmd_set_kind_table() {
  require_pa_deployed
  run_ts scripts/set-kind-table.ts
}

cmd_deny_logic_ref() {
  require_pa_deployed
  run_ts scripts/deny-logic-ref.ts
}

cmd_forwarder() {
  require_cmd npx

  local pid
  pid="$(get_program_id "spl_token_forwarder")"
  require_deployed "$pid" "SPL token forwarder" "deploy stf"
  run_ts scripts/forwarder.ts "$TARGET"
}

cmd_lookup_table() {
  require_pa_deployed
  run_ts scripts/lookup-table.ts
}

cmd_pause() {
  require_pa_deployed
  run_ts scripts/pause-pa.ts pause
}

cmd_unpause() {
  require_pa_deployed
  run_ts scripts/pause-pa.ts unpause
}

cmd_status() {
  echo "=== Status (${CLUSTER}) ==="
  echo ""

  local pubkey balance
  pubkey="$(get_wallet_pubkey)"
  balance="$(get_balance)"
  echo "Wallet: ${pubkey}"
  echo "Balance: ${balance} SOL"
  print_explorer_link "$pubkey"
  echo ""

  for t in "${PROGRAM_TARGETS[@]}"; do
    local name="${PROGRAM_BY_TARGET[$t]}"
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
  if [[ -f "target/deploy/protocol_adapter-keypair.json" ]]; then
    local pa_pid pa_state_addr out
    pa_pid="$(get_program_id "protocol_adapter")"
    pa_state_addr="$(solana find-program-derived-address "$pa_pid" string:pa_state --url "$RPC_URL" | awk 'NR == 1 { print $1 }')"
    if [[ -n "$pa_state_addr" ]]; then
      if out="$(solana account "$pa_state_addr" --url "$RPC_URL" 2>&1)"; then
        echo "PAState PDA: ✅ initialized — ${pa_state_addr}"
      elif [[ "$out" == "Error: AccountNotFound: pubkey=${pa_state_addr}" ]]; then
        echo "PAState PDA: not initialized — ${pa_state_addr}"
      else
        echo "❌ solana account ${pa_state_addr} failed: ${out}" >&2
        exit 1
      fi
    fi
  fi
}

cmd_balance() {
  echo "$(get_wallet_pubkey)  $(get_balance) SOL"
}

# solana-verify maps [workspace.metadata.cli] solana (Cargo.toml) to a build
# image through a table compiled into each release; 0.5.2 is the release whose
# table has the Solana version pinned there.
SOLANA_VERIFY_VERSION="0.5.2"

# Deterministic (verifiable) build of the PA via solana-verify's pinned
# Docker image; with a cluster, also compares against the deployed program's
# hash. The resulting target/deploy/protocol_adapter.so is the artifact that
# must be shipped (deploy/upgrade --prebuilt) for verification to succeed —
# any local rebuild produces different bytes.
cmd_verify_build() {
  # A missing solana-verify fails the substitution ("command not found") and
  # the comparison both.
  if [[ "$(solana-verify --version)" != "solana-verify ${SOLANA_VERIFY_VERSION}" ]]; then
    echo "❌ verify-build needs solana-verify ${SOLANA_VERIFY_VERSION}. Install with:" >&2
    echo "   cargo install solana-verify --version ${SOLANA_VERIFY_VERSION} --locked" >&2
    exit 1
  fi

  checked_sbf_build solana-verify build --library-name protocol_adapter --arch "$SBPF_ARCH"

  local built
  built="$(solana-verify get-executable-hash target/deploy/protocol_adapter.so)"
  echo "Built executable hash: ${built}"

  if [[ -n "$CLUSTER" ]]; then
    local pid deployed
    pid="$(get_program_id "protocol_adapter")"
    deployed="$(solana-verify get-program-hash -u "$RPC_URL" "$pid")"
    echo "Deployed program hash: ${deployed} (${pid}, ${CLUSTER})"
    if [[ "$built" == "$deployed" ]]; then
      echo "✅ Deployed program matches the deterministic build."
    else
      echo "❌ Deployed program does NOT match the deterministic build."
      echo "   Ship the artifact with: ./scripts/dev.sh upgrade pa --cluster ${CLUSTER} --prebuilt"
      exit 1
    fi
  fi
}

# Publish each target program's production IDL on chain as the program's
# canonical Program Metadata "idl" account (derived from the program ID), so
# explorers and generic Anchor clients decode the program's instructions,
# accounts, and events straight from the cluster. Builds the production IDLs
# first — the build self-checks that the dev-only instructions are absent, so
# a dev IDL cannot be published by accident. The program's upgrade authority
# signs the write that creates the account; the account's authority signs
# later ones.
cmd_idl_publish() {
  require_cmd anchor
  # anchor idl runs the Program Metadata client through npx.
  require_cmd npx

  # The Anchor CLI skips IDL writes against a localhost RPC and exits 0, and
  # a local validator has no Program Metadata program to write to.
  if [[ "$CLUSTER" == "localnet" ]]; then
    echo "❌ idl-publish targets devnet or mainnet; a local validator has no Program Metadata program." >&2
    exit 1
  fi

  local targets t
  targets="$(resolve_targets "$TARGET")"
  for t in $targets; do
    require_deployed "$(get_program_id "${PROGRAM_BY_TARGET[$t]}")" "$t" "deploy ${t}"
  done

  build_programs_release

  for t in $targets; do
    publish_idl "${PROGRAM_BY_TARGET[$t]}"
  done
}

# Publish <name>'s production IDL, built by build_programs_release, as the
# program's canonical Program Metadata IDL account, and check the cluster
# serves exactly that file.
publish_idl() {
  local name="$1" pid idl_path="target/idl/${1}.json"
  pid="$(get_program_id "$name")"

  # Program Metadata `write idl`: creates the canonical IDL account on first
  # publish and overwrites it afterwards.
  anchor idl upgrade "$pid" \
    --filepath "$idl_path" \
    --provider.cluster "$RPC_URL" \
    --provider.wallet "$WALLET"

  # Read back what the cluster now serves rather than assuming the write
  # landed; a mismatch here must fail loudly. The fetched document need not
  # keep the file's key order or whitespace, so compare canonicalized JSON.
  local fetched
  fetched="$(mktemp)"
  anchor idl fetch "$pid" --provider.cluster "$RPC_URL" --out "$fetched"
  if ! node -e '
    const fs = require("fs");
    const canon = (v) =>
      Array.isArray(v) ? v.map(canon)
      : v && typeof v === "object"
        ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, canon(v[k])]))
        : v;
    const read = (p) => JSON.stringify(canon(JSON.parse(fs.readFileSync(p, "utf-8"))));
    process.exit(read(process.argv[1]) === read(process.argv[2]) ? 0 : 1);
  ' "$fetched" "$idl_path"; then
    echo "❌ Fetched on-chain IDL does not match ${idl_path}" >&2
    rm -f "$fetched"
    exit 1
  fi
  rm -f "$fetched"
  echo "✅ On-chain IDL for ${pid} matches ${idl_path} (${CLUSTER})"
}

cmd_test() {
  require_cmd yarn
  require_cmd node

  ensure_balance 2

  # Verify the cluster programs are deployed
  local pids=() t pid
  for t in "${PROGRAM_TARGETS[@]}"; do
    pid="$(get_program_id "${PROGRAM_BY_TARGET[$t]}")"
    require_deployed "$pid" "$t" "deploy"
    pids+=("$pid")
  done

  ensure_node_modules
  build_dev_idls

  # The wallet must own none of the programs under test: their owner is their
  # upgrade authority, and a spec signing as the owner could pause the
  # deployment, replace its kind table, or renounce the authority for good.
  # A local validator's deployment is disposable and owned by the local wallet.
  if [[ "$CLUSTER" != "localnet" ]]; then
    run_ts scripts/cluster-test-guard.ts "${pids[@]}"
    # Settlements go through the deployment's own lookup table; without it the
    # suite would create a table of its own on the cluster, and leave it.
    if [[ -z "${PA_SETTLEMENT_TABLE:-}" ]]; then
      echo "❌ Set PA_SETTLEMENT_TABLE to the deployment's settlement lookup table (its deployment record names it)." >&2
      exit 1
    fi
  fi

  # The run proves its own fixture set for the deployment: under a salt no
  # earlier run used, so nothing in it was settled before, and against the
  # kind table the deployment stores. Each fixture is proven when a test
  # first loads it (tests/utils/fixtures.ts), so the run stops at the first
  # failure and proves only what its tests use.
  if [[ -z "${PA_KIND_TABLE:-}" || ! -f "$PA_KIND_TABLE" ]]; then
    echo "❌ Set PA_KIND_TABLE to the kind table (JSON, as fixture-gen reads it) whose commitment the deployment stores;" >&2
    echo "   the run proves its fixtures against it." >&2
    exit 1
  fi
  # PA_FIXTURE_SALT continues an interrupted run's set, whose fixtures were
  # proven but not settled; a fixture it already settled fails loudly as
  # settled.
  local salt fixture_dir
  salt="${PA_FIXTURE_SALT:-${CLUSTER}-$(date -u +%Y%m%dT%H%M%SZ)}"
  fixture_dir="${PROJECT_DIR}/.cache/cluster-fixtures/${salt}"
  mkdir -p "$fixture_dir"
  echo "The run's fixtures (salt ${salt}) are proven into ${fixture_dir} as tests need them"
  # Every spec file loads the suite harness, which reads the primary fixture
  # for the deployment's proof selector before any test runs.
  if [[ ! -f "${fixture_dir}/batch_groth16.json" ]]; then
    "${SCRIPT_DIR}/regen-fixtures.sh" real --out "$fixture_dir" --salt "$salt" --kind-table "$PA_KIND_TABLE" \
      --only batch_groth16.json
  fi

  # The suite's files that build on whatever state they find, or the ones
  # given, each in its own mocha process against the deployment, without the
  # tests tagged @localnet: those need the owner or the forwarder's
  # committee, change a deployment setting, or call a program deployed only
  # on a local validator.
  local specs=()
  if [[ ${#SPEC_FILES[@]} -gt 0 ]]; then
    specs=("${SPEC_FILES[@]}")
  else
    mapfile -t specs < <(history_spec_files)
  fi

  # No per-test timeout (-t 0): a test that first loads a fixture proves it,
  # for as long as proving takes, and a timeout would abandon a test that
  # goes on settling in the background. The RPC calls and confirmations the
  # tests wait on carry their own timeouts.
  echo "Running cluster integration tests (${CLUSTER}): ${specs[*]}"
  local spec
  for spec in "${specs[@]}"; do
    echo "==> ${spec}"
    ANCHOR_PROVIDER_URL="$RPC_URL" \
    ANCHOR_WALLET="$WALLET" \
    PA_TEST_MODE=real \
    PA_FIXTURE_DIR="$fixture_dir" \
    PA_FIXTURE_SALT="$salt" \
    PA_KIND_TABLE="$PA_KIND_TABLE" \
    PA_SETTLEMENT_TABLE="${PA_SETTLEMENT_TABLE:-}" \
      yarn run ts-mocha --type-check -p ./tsconfig.json -t 0 --grep @localnet --invert "$spec"
  done

  echo ""
  echo "✅ Cluster tests passed (${CLUSTER})"
}

# The feature-gated code of a program with dev features compiles only with
# them, so each such program is linted a second time with them enabled.
cmd_clippy() {
  load_workspace_programs
  cargo clippy --workspace --all-targets -- -D warnings
  local name
  for name in "${PROGRAM_NAMES[@]}"; do
    if [[ "${PROGRAM_DEV_FEATURES[$name]}" != "-" ]]; then
      cargo clippy -p "${PROGRAM_PACKAGE[$name]}" --features "${PROGRAM_DEV_FEATURES[$name]}" --all-targets -- -D warnings
    fi
  done
}

# ---------- dispatch ----------

cd "$PROJECT_DIR"

case "$COMMAND" in
  build-dev)
    require_cmd anchor
    if [[ "$NO_IDL" == "true" ]]; then
      build_programs_dev noidl
    else
      build_programs_dev
    fi
    ;;
  sync-ids)
    # Adopt the program IDs in target/deploy/ (restored from git, or freshly
    # committed when rotating to new IDs): sync declare_id! and Anchor.toml,
    # and regenerate the mock verifier-entry fixture that embeds an ID.
    require_cmd anchor
    require_cmd solana-keygen
    require_cmd yarn
    require_cmd node
    sync_program_ids
    ;;
  build-release)
    require_cmd anchor
    require_cmd node
    build_programs_release
    ;;
  clippy)
    require_cmd cargo
    require_cmd node
    cmd_clippy
    ;;
  verify-build)
    if [[ -n "$CLUSTER" ]]; then
      require_cmd solana
      require_cmd solana-keygen
      resolve_cluster
    fi
    cmd_verify_build
    ;;
  refresh-devnet-verifier)
    if [[ -z "$RPC_OVERRIDE" ]]; then
      echo "❌ refresh-devnet-verifier reads devnet through --url <rpc>; there is no default endpoint." >&2
      exit 1
    fi
    require_cmd solana
    require_cmd jq
    refresh_devnet_verifier "$RPC_OVERRIDE"
    ;;
  validator)
    require_cmd solana-test-validator
    # start_validator (validator-deploy.sh) preloads the RISC0 verifier stack
    # copied from devnet and the synthetic verifier-entry account fixtures
    # — a bare validator cannot settle anything.
    require_cmd solana
    start_validator
    trap 'stop_validator' EXIT INT TERM
    echo "Validator running (pid ${VALIDATOR_PID}); log: ${VALIDATOR_LOG}"
    tail -f "$VALIDATOR_LOG"
    ;;
  validator-deploy)
    # Full local stack, kept running: sync IDs, build, start the validator
    # with every program loaded at genesis, then hold it up for external
    # clients (harnesses, manual testing). Ctrl-C tears the validator down.
    require_commands
    ensure_wallet
    ensure_lockfile_sync
    sync_program_ids
    build_programs_dev
    workspace_program_args
    start_validator "${WORKSPACE_PROGRAM_ARGS[@]}"
    trap 'stop_validator' EXIT INT TERM
    echo "Validator running with programs deployed (pid ${VALIDATOR_PID}); log: ${VALIDATOR_LOG}"
    tail -f "$VALIDATOR_LOG"
    ;;
  test)
    # --prebuilt: run the cluster-safe subset against already-deployed
    # programs on the given cluster (including a validator already running
    # on localnet) — the validation path for verify-build artifacts, which
    # the full flow would rebuild and clobber.
    if [[ "$PREBUILT" != "true" && ( -z "$CLUSTER" || "$CLUSTER" == "localnet" ) ]]; then
      # Full deterministic local flow: sync IDs, build, then the spec files
      # (all, or the ones given) on one validator with the programs loaded at
      # genesis. Guard against Cargo.lock skew first.
      ensure_lockfile_sync
      PA_TEST_MODE="$TEST_MODE" exec "${SCRIPT_DIR}/anchor-test.sh" all "${SPEC_FILES[@]}"
    fi
    # Everything below is the cluster-subset path, where the mock verifier
    # is never deployed.
    if [[ "$TEST_MODE" == "mock" ]]; then
      echo "❌ Mock mode (--mode mock or PA_TEST_MODE=mock) is localnet-only (the mock verifier never deploys to a real cluster)" >&2
      exit 1
    fi
    require_cmd solana
    require_cmd solana-keygen
    resolve_cluster
    cmd_test
    ;;
  deploy|upgrade|init|set-kind-table|deny-logic-ref|forwarder|lookup-table|pause|unpause|status|balance|idl-publish)
    require_cmd solana
    require_cmd solana-keygen
    resolve_cluster
    "cmd_${COMMAND//-/_}"
    ;;
  *)
    echo "❌ Unknown command: ${COMMAND}" >&2
    usage
    ;;
esac
