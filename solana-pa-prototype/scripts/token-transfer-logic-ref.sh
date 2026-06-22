#!/usr/bin/env bash

# TOKEN_TRANSFER_ID from anomapay-backend transfer_library/src/lib.rs.
# This is the logic_ref authorized by the SPL token forwarder config.
TOKEN_TRANSFER_LOGIC_REF="379ef89a9471c6db8bf60d7fe06779b386b036018660c97c2b6a2a4a64f5a2f7"

require_token_transfer_logic_ref() {
  if [[ ! "${TOKEN_TRANSFER_LOGIC_REF:-}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "Invalid TOKEN_TRANSFER_LOGIC_REF: expected 64 hex characters" >&2
    exit 1
  fi
}
