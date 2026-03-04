//! External call encoding, decoding, and CPI execution.

// CPI module excluded from test builds — Solana invoke syscalls are unavailable.
#[cfg(not(test))]
mod cpi;

#[cfg(not(test))]
pub use cpi::execute_external_calls;

use crate::error::PAError;
use crate::types::{ExpirableBlob, SolanaExternalCall};
#[cfg(test)]
use arm_core::utils::bytes_to_words;
use arm_core::utils::words_to_bytes;

/// Test-only: the on-chain program decodes external calls, never encodes them.
#[cfg(test)]
pub(crate) fn encode_external_call(call: &SolanaExternalCall) -> ExpirableBlob {
    let bytes = bincode::serialize(call).expect("serialization should not fail");
    ExpirableBlob {
        blob: bytes_to_words(&bytes),
        // 0 = "Immediately" (ephemeral, not persisted as an event)
        deletion_criterion: 0,
    }
}

/// Decode an external call from its word-array blob.
pub fn decode_external_call(blob: &ExpirableBlob) -> Result<SolanaExternalCall, PAError> {
    let bytes = words_to_bytes(&blob.blob);
    bincode::deserialize(bytes).map_err(|_| PAError::InvalidExternalCallBlob)
}

/// Verify that actual output matches expected output.
pub fn verify_output(expected: &[u8], actual: &[u8]) -> Result<(), PAError> {
    if expected != actual {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    Ok(())
}

/// Anchor discriminator for BlockTimeForwarder::forward_call (sha256("global:forward_call")[..8])
pub const FORWARD_CALL_DISCRIMINATOR: [u8; 8] = hex_literal::hex!("9faae00afd696cde");

/// Build instruction data for a forwarder's forward_call instruction.
/// Format: discriminator (8 bytes) + logic_ref (32 bytes) + input_len (4 bytes) + input (N bytes)
pub fn build_forwarder_instruction_data(logic_ref: &[u8; 32], input: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + 32 + 4 + input.len());

    // Discriminator
    data.extend_from_slice(&FORWARD_CALL_DISCRIMINATOR);

    // logic_ref (32 bytes)
    data.extend_from_slice(logic_ref);

    // input length as u32 little-endian (Borsh format)
    data.extend_from_slice(&(input.len() as u32).to_le_bytes());

    // input data
    data.extend_from_slice(input);

    data
}
