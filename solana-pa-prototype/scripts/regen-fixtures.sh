#!/usr/bin/env bash
# Regenerate the complete fixture set for one proof mode, or one fixture of it.
#
# Usage: regen-fixtures.sh <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH] [--only FILE]
#   real — full RISC0 proving (succinct base proofs + Groth16 aggregation via
#          a container runtime, or the workers queue at QUEUE_BASE_URL).
#          Writes tests/fixtures/. Hours of CPU.
#   mock — dev-mode execution (guests run, nothing proven; mock seals only the
#          localnet mock verifier accepts). Writes tests/fixtures/mock/. Seconds.
#   --out DIR         write to DIR instead
#   --salt SALT       derive every nonce from SALT too, so nothing proven was
#                     settled before on the deployment it is proven for
#   --kind-table PATH prove against that kind table instead of the committed
#                     empty one; the deployment must store its commitment
#   --only FILE       prove only the recipe that writes FILE (an error variant
#                     comes with its base); fixtures it spends from must exist
# A cluster run proves each fixture with the first three, and --only, when a
# test first loads it (tests/utils/fixtures.ts).
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
# nullifier. A spend through a Merkle path (the historical-root consumer) is
# proven over the tree the fresh phase (tests/fresh/) builds in its fixed
# order.
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
    echo "Usage: $0 <real|mock> [--out DIR] [--salt SALT] [--kind-table PATH] [--only FILE]" >&2
    exit 1
    ;;
esac
shift

SALT_FLAG=()
KIND_TABLE_FLAG=()
ONLY=""
while [[ $# -gt 0 ]]; do
  [[ $# -ge 2 ]] || { echo "❌ $1 requires a value" >&2; exit 1; }
  case "$1" in
    --out) OUT_DIR="$2" ;;
    --salt) SALT_FLAG=(--salt "$2") ;;
    --kind-table) KIND_TABLE_FLAG=(--kind-table "$2") ;;
    --only) ONLY="$2" ;;
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

# Whether this run proves the recipe that writes the files named: every
# recipe, or the one writing --only.
PROVEN=0
wanted() {
  if [[ -z "$ONLY" ]]; then
    return 0
  fi
  local file
  for file in "$@"; do
    if [[ "$file" == "$ONLY" ]]; then
      PROVEN=1
      return 0
    fi
  done
  return 1
}

# Settled by one test each.
for name in batch_groth16 batch_groth16_v2 batch_groth16_v3 batch_groth16_unpaused batch_groth16_denylist; do
  if wanted "$name.json"; then gen batch "$OUT_DIR/$name.json"; fi
done
if wanted batch_groth16_multi_call.json; then
  gen batch --multi-external-call "$OUT_DIR/batch_groth16_multi_call.json"
fi
if wanted batch_groth16_transfer_shape.json; then
  gen transfer-shape "$OUT_DIR/batch_groth16_transfer_shape.json"
fi
if wanted batch_groth16_consume_only.json; then
  gen consume-only "$OUT_DIR/batch_groth16_consume_only.json"
fi

# The fresh phase's settlements, in their order: the committer (leaf 0) and
# its successor, then the consumer spending the committer's resource through
# its retained root (tests/fresh/3-historical-root.ts).
COMMITTER="$OUT_DIR/batch_groth16_historical_root_committer.json"
SUCCESSOR="$OUT_DIR/batch_groth16_historical_root_successor.json"
CONSUMER="$OUT_DIR/batch_groth16_historical_root.json"
if wanted batch_groth16_historical_root_committer.json; then gen historical-root-committer "$COMMITTER"; fi
if wanted batch_groth16_historical_root_successor.json; then gen batch "$SUCCESSOR"; fi
if wanted batch_groth16_historical_root.json; then
  gen historical-root-consumer --committer "$COMMITTER" "$CONSUMER"
fi

# Settled by whichever test first needs a settled fixture to resubmit.
if wanted batch_groth16_resubmitted.json; then gen batch "$OUT_DIR/batch_groth16_resubmitted.json"; fi

# Never settled: rejections that fail after the nullifiers are recorded need
# them unspent. The error variants are mutations of this transaction.
if wanted batch_groth16_rejected.json wrong_root.json no_aggregation.json garbage_proof.json corrupt_seal.json \
  zero_action.json witness_delta.json; then
  gen batch --error-variants "$OUT_DIR" "$OUT_DIR/batch_groth16_rejected.json"
fi
if wanted batch_groth16_mismatch.json; then gen output-mismatch "$OUT_DIR/batch_groth16_mismatch.json"; fi
if wanted batch_forwarder_fail.json; then gen forwarder-fail "$OUT_DIR/batch_forwarder_fail.json"; fi
if wanted batch_forwarder_silent.json; then gen forwarder-silent "$OUT_DIR/batch_forwarder_silent.json"; fi

if [[ -n "$ONLY" ]]; then
  if [[ "$PROVEN" -ne 1 ]]; then
    echo "❌ No recipe writes ${ONLY}" >&2
    exit 1
  fi
  echo "✅ Proved ${ONLY} (${MODE}) in ${OUT_DIR}"
else
  echo "✅ Regenerated the $MODE fixture set in $OUT_DIR"
fi
