#!/usr/bin/env bash
# Regenerate idls/verifier_router.json and idls/groth_16_verifier.json, the
# IDLs `declare_program!` reads to generate the RISC Zero verifier CPI
# clients and types.
#
# Neither deployed verifier program publishes an on-chain IDL, and the IDL
# committed in risc0-solana at v3.0.0 does not match that tag's source, so
# the IDLs are built from the v3.0.0 program source itself.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

REPO_URL="https://github.com/risc0/risc0-solana.git"
TAG="v3.0.0"
COMMIT="ee415935d04a948f27a346b563391900bdad6486"
CHECKOUT="${PROJECT_DIR}/.cache/risc0-solana"
IDL_DIR="${PROJECT_DIR}/idls"

if [[ ! -d "${CHECKOUT}/.git" ]]; then
  mkdir -p "$(dirname "$CHECKOUT")"
  git clone --quiet "$REPO_URL" "$CHECKOUT"
fi
git -C "$CHECKOUT" fetch --quiet --tags origin
git -C "$CHECKOUT" checkout --quiet --detach "refs/tags/${TAG}"

actual="$(git -C "$CHECKOUT" rev-parse HEAD)"
if [[ "$actual" != "$COMMIT" ]]; then
  echo "❌ risc0-solana tag ${TAG} resolves to ${actual}, expected ${COMMIT}" >&2
  exit 1
fi

# verifier_router reads INITIAL_OWNER at compile time into a constant that is
# not part of its IDL; any valid pubkey lets the program compile.
export INITIAL_OWNER="11111111111111111111111111111111"

# --no-docs: declare_program! turns IDL docs into rustdoc, and the upstream
# docs carry code examples that would compile as this crate's doctests.
mkdir -p "$IDL_DIR"
for program in verifier_router groth_16_verifier; do
  echo "==> ${program}"
  (cd "${CHECKOUT}/solana-verifier" && anchor idl build --no-docs -p "$program" -o "${IDL_DIR}/${program}.json")
done

echo "✅ IDLs written to ${IDL_DIR} from risc0-solana ${TAG} (${COMMIT})"
