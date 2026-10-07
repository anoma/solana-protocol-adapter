//! External call encoding, decoding, and CPI execution.

// CPI module excluded from test builds — Solana invoke syscalls are unavailable.
#[cfg(not(test))]
mod cpi;

#[cfg(not(test))]
pub use cpi::ForwarderSegments;

use crate::error::PAError;
use crate::types::SolanaExternalCall;
use anchor_lang::prelude::{AccountInfo, Pubkey};
use anchor_lang::solana_program::instruction::AccountMeta;
use arm_core::logic_instance::{AppData, ExpirableBlob};
use arm_core::utils::bytes_to_words;
use arm_core::utils::words_to_bytes;

/// Encode a SolanaExternalCall into an ExpirableBlob (word-array format).
/// The on-chain program only decodes; this is used by tests and fixture-gen.
pub fn encode_external_call(call: &SolanaExternalCall) -> ExpirableBlob {
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

/// The output of a `forward_call` to `forwarder`: the `Vec<u8>` it returned,
/// Borsh-encoded as Anchor encodes a returned value, as pa-evm's forwarder
/// call returns `bytes`. The length prefix makes an empty output four bytes
/// of return data, so it differs from no return data, which the runtime
/// reports for a forwarder that set none; that, return data from another
/// program, or anything but one encoded `Vec<u8>` is a mismatch.
pub fn decode_forwarder_output(
    return_data: Option<(Pubkey, Vec<u8>)>,
    forwarder: &Pubkey,
) -> Result<Vec<u8>, PAError> {
    let (program_id, mut data) = return_data.ok_or(PAError::ForwarderCallOutputMismatch)?;
    let encodes_one_vec = data
        .split_first_chunk::<4>()
        .is_some_and(|(len, output)| u32::from_le_bytes(*len) as usize == output.len());
    if program_id != *forwarder || !encodes_one_vec {
        return Err(PAError::ForwarderCallOutputMismatch);
    }
    // Decoded in place: the settlement's heap never frees, so a decoded copy
    // would stay allocated.
    data.drain(..4);
    Ok(data)
}

/// Verify that actual output matches expected output.
pub fn verify_output(expected: &[u8], actual: &[u8]) -> Result<(), PAError> {
    if expected != actual {
        return Err(PAError::ForwarderCallOutputMismatch);
    }
    Ok(())
}

/// A resource's external calls, decoded in the order its app data lists them.
pub fn decode_external_calls(app_data: &AppData) -> Result<Vec<SolanaExternalCall>, PAError> {
    app_data
        .external_payload
        .iter()
        .map(decode_external_call)
        .collect()
}

/// Build the account metas for a forwarder CPI from its segment.
///
/// `segment[0]` is the forwarder program itself; the metas cover the rest.
///
/// Signer authority is never propagated. Solana grants an account the highest
/// privilege it holds anywhere in the message, so an outer signer appearing in a
/// forwarded position would otherwise reach the forwarder as a signer. Signing a
/// settlement authorizes submission, not action by whatever program the proof
/// names. Writability is passed through: forwarders legitimately need writable
/// accounts, and the proof does not yet bind which (finding EXT-06).
pub fn build_account_metas(segment: &[AccountInfo<'_>]) -> Vec<AccountMeta> {
    segment[1..]
        .iter()
        .map(|ai| AccountMeta {
            pubkey: *ai.key,
            is_signer: false,
            is_writable: ai.is_writable,
        })
        .collect()
}

/// Anchor discriminator of every forwarder's `forward_call` (sha256("global:forward_call")[..8])
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
