#!/usr/bin/env bash
# Regenerate the complete fixture set for one proof mode.
#
# Usage: regen-fixtures.sh <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH]
#   real — full RISC0 proving (succinct base proofs + Groth16 aggregation via
#          a container runtime, or the workers queue at QUEUE_BASE_URL).
#          Writes tests/fixtures/. Hours of CPU.
#   mock — dev-mode execution (guests run, nothing proven; mock seals only the
#          localnet mock verifier accepts). Writes tests/fixtures/mock/. Seconds.
#   --out DIR         write the set to DIR instead
#   --salt SALT       derive every nonce from SALT too, so nothing in the set
#                     was settled before on the deployment it is proven for
#   --kind-table PATH prove against that kind table instead of the committed
#                     empty one; the deployment must store its commitment
# A cluster run proves its set with all three (ops.sh test).
#
# Runs are strictly SEQUENTIAL: each RISC0 proving run consumes most of the
# machine's CPU and memory, and parallel proving crashes the machine.
#
# Every fixture variant lives here so the set cannot drift. The suite runs on
# one deployment, so each fixture has one role: settled by exactly one test,
# settled by whichever test needs it settled first (resubmitted), or never
# settled (rejected, its error variants, and the deliberate-failure
# forwarder fixtures). fixture-gen derives every resource nonce from the
# output file's name, so fixtures with different names never share a
# nullifier. A spend through a Merkle path (the unwraps, the historical-root
# consumer) depends on the tree the deployment holds when the spent resource
# settles, so the suite proves it while it runs.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$PROJECT_DIR"

MODE="${1:-}"
case "$MODE" in
  real)
    OUT_DIR="tests/fixtures"
    MODE_FLAG=()
    ;;
  mock)
    OUT_DIR="tests/fixtures/mock"
    MODE_FLAG=(--mock)
    ;;
  *)
    echo "Usage: $0 <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH]" >&2
    exit 1
    ;;
esac
shift

SALT_FLAG=()
KIND_TABLE_FLAG=()
while [[ $# -gt 0 ]]; do
  [[ $# -ge 2 ]] || { echo "❌ $1 requires a value" >&2; exit 1; }
  case "$1" in
    --out) OUT_DIR="$2" ;;
    --salt) SALT_FLAG=(--salt "$2") ;;
    --kind-table) KIND_TABLE_FLAG=(--kind-table "$2") ;;
    *)
      echo "❌ Unknown argument: $1" >&2
      exit 1
      ;;
  esac
  shift 2
done

# One fixture: the shape, the run's mode and salt, then the shape's own
# arguments. `gen` proves against the run's kind table.
gen_with() {
  local shape="$1"
  shift
  echo "==> fixture-gen ${shape} $*"
  cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- \
    "$shape" "${MODE_FLAG[@]}" "${SALT_FLAG[@]}" "$@"
}
gen() {
  local shape="$1"
  shift
  gen_with "$shape" "${KIND_TABLE_FLAG[@]}" "$@"
}

# Settled by one test each.
gen batch "$OUT_DIR/batch_groth16.json"
gen batch "$OUT_DIR/batch_groth16_v2.json"
gen batch "$OUT_DIR/batch_groth16_v3.json"
gen batch --multi-external-call "$OUT_DIR/batch_groth16_multi_call.json"
gen batch "$OUT_DIR/batch_groth16_unpaused.json"
gen batch "$OUT_DIR/batch_groth16_denylist.json"
gen batch "$OUT_DIR/batch_groth16_historical_root_successor.json"
gen transfer-shape "$OUT_DIR/batch_groth16_transfer_shape.json"
gen consume-only "$OUT_DIR/batch_groth16_consume_only.json"
# Creates the non-ephemeral resource the historical-root consumer spends.
gen historical-root-committer "$OUT_DIR/batch_groth16_historical_root_committer.json"

# Settled by whichever test first needs a settled fixture to resubmit.
gen batch "$OUT_DIR/batch_groth16_resubmitted.json"

# Never settled: rejections that fail after the nullifiers are recorded need
# them unspent. The error variants are mutations of this transaction.
gen batch --error-variants "$OUT_DIR" "$OUT_DIR/batch_groth16_rejected.json"
gen output-mismatch "$OUT_DIR/batch_groth16_mismatch.json"
gen forwarder-fail "$OUT_DIR/batch_forwarder_fail.json"
gen forwarder-silent "$OUT_DIR/batch_forwarder_silent.json"
gen forwarder-relay "$OUT_DIR/batch_forwarder_relay.json"

# The AnomaPay fixtures are proven with the real transfer logic: the wrap,
# and the same wrap under a fresh nullifier (a replay of its forwarder nonce).
gen spl-token-wrap "$OUT_DIR/spl_token_wrap.json"
gen spl-token-wrap "$OUT_DIR/spl_token_wrap_replay.json"

# A second wrap (the next forwarder nonce) proven against the solana-devnet
# kind table from anoma/risc0-kind-tables (data/generated/staging), whatever
# table the run proves against, settled after set_kind_table_commitment
# installs that table.
gen_with spl-token-wrap --wrap-nonce 2 \
  --kind-table tools/fixture-gen/kind_table_solana_devnet.json \
  "$OUT_DIR/spl_token_wrap_devnet_kind_table.json"

echo "✅ Regenerated the $MODE fixture set in $OUT_DIR"
