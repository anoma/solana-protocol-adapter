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
# variants, the duplicates, and the historical-root pair. fixture-gen
# derives every resource nonce from the output file's name, so fixtures
# with different names never share a nullifier.
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

gen batch "${MOCK_FLAG[@]}" --error-variants "$OUT_DIR" "$OUT_DIR/batch_groth16.json"
gen output-mismatch "${MOCK_FLAG[@]}" "$OUT_DIR/batch_groth16_mismatch.json"
gen batch "${MOCK_FLAG[@]}" "$OUT_DIR/batch_groth16_v2.json"
gen batch "${MOCK_FLAG[@]}" "$OUT_DIR/batch_groth16_v3.json"
gen batch "${MOCK_FLAG[@]}" --multi-external-call "$OUT_DIR/batch_groth16_multi_call.json"
gen forwarder-fail "${MOCK_FLAG[@]}" "$OUT_DIR/batch_forwarder_fail.json"
gen forwarder-silent "${MOCK_FLAG[@]}" "$OUT_DIR/batch_forwarder_silent.json"
gen forwarder-relay "${MOCK_FLAG[@]}" "$OUT_DIR/batch_forwarder_relay.json"
gen transfer-shape "${MOCK_FLAG[@]}" "$OUT_DIR/batch_groth16_transfer_shape.json"
gen consume-only "${MOCK_FLAG[@]}" "$OUT_DIR/batch_groth16_consume_only.json"

# The historical-root pair is proven over the tree [batch_groth16,
# committer]: its spec file settles exactly those two first, so the
# committer lands at leaf 1 and the consumer spends it through a real
# Merkle path.
gen historical-root "$OUT_DIR/batch_groth16.json" \
  "$OUT_DIR/batch_groth16_historical_root_committer.json" \
  "$OUT_DIR/batch_groth16_historical_root.json" \
  "${MOCK_FLAG[@]}"

# The AnomaPay fixtures are proven with the real transfer logic: the wrap,
# the same wrap under a fresh nullifier (a replay of its forwarder nonce),
# and the unwrap. The unwrap consumes the resource the wrap creates, through
# a Merkle path over a fresh adapter's tree holding only the wrap: its spec
# file settles the wrap and nothing else before it.
gen spl-token-wrap "${MOCK_FLAG[@]}" "$OUT_DIR/spl_token_wrap.json"
gen spl-token-wrap "${MOCK_FLAG[@]}" "$OUT_DIR/spl_token_wrap_replay.json"
gen spl-token-unwrap "${MOCK_FLAG[@]}" \
  --wrap "$OUT_DIR/spl_token_wrap.json" \
  "$OUT_DIR/spl_token_unwrap.json"
gen spl-token-unwrap "${MOCK_FLAG[@]}" --to-escrow \
  --wrap "$OUT_DIR/spl_token_wrap.json" \
  "$OUT_DIR/spl_token_unwrap_to_escrow.json"

# A second wrap (forwarder nonce 2) proven against the solana-devnet kind
# table from anoma/risc0-kind-tables (data/generated/staging), settled
# after set_kind_table_commitment installs that table.
gen spl-token-wrap "${MOCK_FLAG[@]}" --wrap-nonce 2 \
  --kind-table tools/fixture-gen/kind_table_solana_devnet.json \
  "$OUT_DIR/spl_token_wrap_devnet_kind_table.json"

echo "✅ Regenerated the $MODE fixture set in $OUT_DIR"
