//! External call encoding, decoding, and CPI execution.

// CPI module excluded from test builds — Solana invoke syscalls are unavailable.
#[cfg(not(test))]
mod cpi;

#[cfg(not(test))]
pub use cpi::execute_external_calls;

use crate::error::PAError;
use crate::types::SolanaExternalCall;
use anchor_lang::prelude::AccountInfo;
use anchor_lang::solana_program::instruction::AccountMeta;
use arm_core::aggregation_instance::AggregationInstance;
use arm_core::logic_instance::ExpirableBlob;
use arm_core::utils::bytes_to_words;
use arm_core::utils::words_to_bytes;
use arm_core::Digest;

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
    let call: SolanaExternalCall =
        bincode::deserialize(bytes).map_err(|_| PAError::InvalidExternalCallBlob)?;

    // Solana reports no return-data record for both `set_return_data(&[])` and a
    // silent return, so an authorized empty output is unrepresentable. Reject it
    // here rather than failing later as an output mismatch. Absence of return
    // data must stay an error, never another spelling of empty.
    if call.expected_output.is_empty() {
        return Err(PAError::EmptyExpectedOutput);
    }

    Ok(call)
}

/// Verify that actual output matches expected output.
pub fn verify_output(expected: &[u8], actual: &[u8]) -> Result<(), PAError> {
    if expected != actual {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    Ok(())
}

/// Extract external calls from the aggregation instance, in instance order:
/// actions in sequence, consumed resources before created resources within
/// each action.
///
/// The instance is the single authority over effect order — its serialization
/// is what the journal digest (and therefore the Groth16 proof) commits to,
/// so no independently ordered wire structure can reorder effects.
pub fn extract_external_calls(
    instance: &AggregationInstance,
) -> Result<Vec<(Digest, SolanaExternalCall)>, PAError> {
    let total: usize = instance
        .actions
        .iter()
        .map(|a| {
            a.consumed_publics
                .iter()
                .map(|c| c.app_data.external_payload.len())
                .sum::<usize>()
                + a.created_publics
                    .iter()
                    .map(|c| c.app_data.external_payload.len())
                    .sum::<usize>()
        })
        .sum();
    let mut calls = Vec::with_capacity(total);

    for action in &instance.actions {
        for consumed in &action.consumed_publics {
            for blob in &consumed.app_data.external_payload {
                calls.push((consumed.resource_logic_ref, decode_external_call(blob)?));
            }
        }
        for created in &action.created_publics {
            for blob in &created.app_data.external_payload {
                calls.push((created.resource_logic_ref, decode_external_call(blob)?));
            }
        }
    }

    Ok(calls)
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
