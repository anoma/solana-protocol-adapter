#!/usr/bin/env bash
# Regenerate the complete fixture set for one proof mode.
#
# Usage: regen-fixtures.sh <real|mock>
#   real — full RISC0 proving (succinct base proofs + Groth16 aggregation via
#          a container runtime). Writes tests/fixtures/. Hours of CPU.
#   mock — dev-mode execution (guests run, nothing proven; mock seals only the
#          localnet mock verifier accepts). Writes tests/fixtures/mock/. Seconds.
#
# Runs are strictly SEQUENTIAL: each RISC0 proving run consumes most of the
# machine's CPU and memory, and parallel proving crashes the machine.
#
# Every fixture variant lives here so the set cannot drift: the primary
# fixture (plus its error variants), the deliberate-failure forwarder
# variants, the nonce-seeded duplicates, and the historical-root pair.
# Nonce seeds keep the variants' nullifiers distinct (fixture-gen reserves
# 8 for the historical-root committer).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$PROJECT_DIR"

MODE="${1:-}"
case "$MODE" in
  real)
    OUT_DIR="tests/fixtures"
    MOCK_FLAG=()
    ;;
  mock)
    OUT_DIR="tests/fixtures/mock"
    MOCK_FLAG=(--mock)
    ;;
  *)
    echo "Usage: $0 <real|mock>" >&2
    exit 1
    ;;
esac

gen() {
  echo "==> fixture-gen $*"
  cargo run --release --manifest-path tools/fixture-gen/Cargo.toml -- "$@"
}

gen "${MOCK_FLAG[@]}" --error-variants "$OUT_DIR" "$OUT_DIR/batch_groth16.json"
gen "${MOCK_FLAG[@]}" --output-mismatch "$OUT_DIR/batch_groth16_mismatch.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 3 "$OUT_DIR/batch_groth16_v2.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 4 "$OUT_DIR/batch_groth16_v3.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 5 --multi-external-call "$OUT_DIR/batch_groth16_multi_call.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 6 --forwarder-fail "$OUT_DIR/batch_forwarder_fail.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 7 --forwarder-silent "$OUT_DIR/batch_forwarder_silent.json"
gen "${MOCK_FLAG[@]}" --nonce-seed 23 --forwarder-relay "$OUT_DIR/batch_forwarder_relay.json"
gen "${MOCK_FLAG[@]}" --transfer-shape "$OUT_DIR/batch_groth16_transfer_shape.json"

# The historical-root pair: the committer lands at leaf 1, right after
# batch_groth16, and the consumer spends it through a real Merkle path.
gen historical-root "$OUT_DIR/batch_groth16.json" \
  "$OUT_DIR/batch_groth16_historical_root_committer.json" \
  "$OUT_DIR/batch_groth16_historical_root.json" \
  "${MOCK_FLAG[@]}"

# The AnomaPay fixtures are proven with the real transfer logic: the wrap,
# the same wrap under a fresh nullifier (a replay of its nonce), and the
# unwrap. The unwrap consumes the resource the wrap creates, through a
# Merkle path over the commitments the suite settles before it, in
# settlement order: keep this list equal to the suite's order
# (tests/solana-pa-prototype.ts).
gen "${MOCK_FLAG[@]}" --spl-token-wrap "$OUT_DIR/spl_token_wrap.json"
gen "${MOCK_FLAG[@]}" --spl-token-wrap --nonce-seed 21 "$OUT_DIR/spl_token_wrap_replay.json"
SETTLED_BEFORE_UNWRAP=(
  batch_groth16.json
  batch_groth16_historical_root_committer.json
  batch_groth16_transfer_shape.json
  batch_groth16_v2.json
  batch_groth16_v3.json
  batch_groth16_multi_call.json
  batch_groth16_historical_root.json
  spl_token_wrap.json
)
gen "${MOCK_FLAG[@]}" --spl-token-unwrap \
  "${SETTLED_BEFORE_UNWRAP[@]/#/--settled=$OUT_DIR/}" \
  "$OUT_DIR/spl_token_unwrap.json"

# A second wrap (forwarder nonce 2) proven against the solana-devnet kind
# table from anoma/risc0-kind-tables (data/generated/staging), which the
# suite installs with set_kind_table_commitment after the unwrap.
gen "${MOCK_FLAG[@]}" --spl-token-wrap --nonce-seed 22 --wrap-nonce 2 \
  --kind-table tools/fixture-gen/kind_table_solana_devnet.json \
  "$OUT_DIR/spl_token_wrap_devnet_kind_table.json"

echo "✅ Regenerated the $MODE fixture set in $OUT_DIR"
