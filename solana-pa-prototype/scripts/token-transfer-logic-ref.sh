#!/usr/bin/env bash

# TOKEN_TRANSFER_ID from anomapay-backend transfer_library/src/lib.rs.
# This is the logic_ref authorized by the SPL token forwarder config.
TOKEN_TRANSFER_LOGIC_REF="9bda007dd983c27f733663dc3c84a49e14dcf5a6d65958494f51ccb77fd8ed84"

require_token_transfer_logic_ref() {
  if [[ ! "${TOKEN_TRANSFER_LOGIC_REF:-}" =~ ^[0-9a-fA-F]{64}$ ]]; then
    echo "Invalid TOKEN_TRANSFER_LOGIC_REF: expected 64 hex characters" >&2
    exit 1
  fi
}
