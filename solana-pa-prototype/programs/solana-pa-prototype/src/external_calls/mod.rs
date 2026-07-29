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
use arm_core::logic_instance::ExpirableBlob;
use arm_core::transaction::Transaction;
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

/// Extract external calls from a transaction in compliance-tag order.
///
/// The aggregation journal is built by walking each action's compliance units and
/// looking up the matching logic input by tag (`encoding.rs`). Execution order
/// must come from that same traversal, otherwise the serialized order of
/// `logic_verifier_inputs` becomes a second authority over effects that the proof
/// does not bind.
pub fn extract_external_calls(
    tx: &Transaction,
) -> Result<Vec<(Digest, SolanaExternalCall)>, PAError> {
    let total: usize = tx
        .actions
        .iter()
        .flat_map(|a| &a.logic_verifier_inputs)
        .map(|lvi| lvi.app_data.external_payload.len())
        .sum();
    let mut calls = Vec::with_capacity(total);

    for action in &tx.actions {
        crate::encoding::visit_logic_inputs_in_tag_order(
            action,
            |_idx, _action_tree_root, lvi| {
                for blob in &lvi.app_data.external_payload {
                    calls.push((lvi.verifying_key, decode_external_call(blob)?));
                }
                Ok(())
            },
        )?;
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
