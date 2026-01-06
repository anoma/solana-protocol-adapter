//! External call encoding, decoding, and CPI execution.

use crate::encoding::{bytes_to_words, words_to_bytes};
use crate::error::PAError;
use crate::types::{ExpirableBlob, OutputMode, SolanaExternalCall};

/// Encode an external call into an ExpirableBlob.
/// Serializes using bincode and converts to word array.
pub fn encode_external_call(call: &SolanaExternalCall) -> ExpirableBlob {
    let bytes = bincode::serialize(call).expect("serialization should not fail");
    ExpirableBlob {
        blob: bytes_to_words(&bytes),
        deletion_criterion: 0,
    }
}

/// Decode an external call from an ExpirableBlob.
/// Converts from word array and deserializes using bincode.
pub fn decode_external_call(blob: &ExpirableBlob) -> Result<SolanaExternalCall, PAError> {
    let bytes = words_to_bytes(&blob.blob);
    bincode::deserialize(&bytes).map_err(|_| PAError::InvalidExternalCallBlob)
}

/// Verify that actual output matches expected output.
pub fn verify_output(expected: &[u8], actual: &[u8], _mode: &OutputMode) -> Result<(), PAError> {
    if expected.len() != actual.len() {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    if expected != actual {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    Ok(())
}

// =============================================================================
// External Call Execution
// =============================================================================

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

// =============================================================================
// Segment Finding
// =============================================================================

#[cfg(not(test))]
use anchor_lang::prelude::Pubkey;

/// Find the account segment for a forwarder in remaining_accounts.
///
/// Segments start with the forwarder program account, followed by any CPI accounts
/// the forwarder needs. Segments appear in the same order as external calls.
///
/// Returns (seg_start, seg_end) indices into external_accounts.
#[cfg(not(test))]
fn find_forwarder_segment(
    external_accounts: &[anchor_lang::prelude::AccountInfo],
    cursor: usize,
    program_id: &Pubkey,
    call_programs: &[Pubkey],
) -> Result<(usize, usize), PAError> {
    let seg_start_rel = external_accounts[cursor..]
        .iter()
        .position(|a| a.key == program_id)
        .ok_or(PAError::UnregisteredForwarder)?;
    let seg_start = cursor + seg_start_rel;

    let mut seg_end = external_accounts.len();
    for i in (seg_start + 1)..external_accounts.len() {
        if call_programs.iter().any(|p| external_accounts[i].key == p) {
            seg_end = i;
            break;
        }
    }

    Ok((seg_start, seg_end))
}

// =============================================================================
// Forwarder Invocation
// =============================================================================

/// Invoke a forwarder program via CPI.
#[cfg(not(test))]
fn invoke_forwarder<'info>(
    program_id: Pubkey,
    logic_ref: &[u8; 32],
    instruction_data: &[u8],
    forwarder_program_info: &anchor_lang::prelude::AccountInfo<'info>,
    cpi_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
) -> Result<(), PAError> {
    use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
    use anchor_lang::solana_program::program::invoke;

    let ix_data = build_forwarder_instruction_data(logic_ref, instruction_data);

    let ix_accounts: Vec<AccountMeta> = cpi_accounts
        .iter()
        .map(|ai| AccountMeta {
            pubkey: *ai.key,
            is_signer: ai.is_signer,
            is_writable: ai.is_writable,
        })
        .collect();

    let ix = Instruction {
        program_id,
        accounts: ix_accounts,
        data: ix_data,
    };

    let mut invoke_infos: Vec<anchor_lang::prelude::AccountInfo<'info>> =
        Vec::with_capacity(1 + cpi_accounts.len());
    invoke_infos.push(forwarder_program_info.clone());
    invoke_infos.extend_from_slice(cpi_accounts);

    invoke(&ix, &invoke_infos)?;
    Ok(())
}

// =============================================================================
// Output Reading
// =============================================================================

/// Read forwarder output based on the output mode.
#[cfg(not(test))]
fn read_forwarder_output<'info>(
    output_mode: &OutputMode,
    program_id: &Pubkey,
    remaining_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
) -> Result<Vec<u8>, PAError> {
    use anchor_lang::solana_program::program::get_return_data;

    match output_mode {
        OutputMode::ReturnData => {
            let (returned_program_id, return_data) =
                get_return_data().ok_or(PAError::ExternalCallOutputMismatch)?;
            if returned_program_id != *program_id {
                return Err(PAError::ExternalCallOutputMismatch);
            }
            Ok(return_data)
        }
        OutputMode::OutputAccount { index, offset, len } => {
            let idx = *index as usize;
            if idx >= remaining_accounts.len() {
                return Err(PAError::ExternalCallOutputMismatch);
            }
            let data = remaining_accounts[idx].data.borrow();
            let start = *offset as usize;
            let end = start
                .checked_add(*len as usize)
                .ok_or(PAError::ExternalCallOutputMismatch)?;
            if end > data.len() {
                return Err(PAError::ExternalCallOutputMismatch);
            }
            Ok(data[start..end].to_vec())
        }
    }
}

// =============================================================================
// Single Call Execution (analog to EVM's _executeForwarderCall)
// =============================================================================

/// Execute a single external call via CPI.
///
/// This is the Solana analog to EVM's `_executeForwarderCall`:
/// 1. Invoke forwarder via CPI
/// 2. Read and verify output
/// 3. Emit event
///
/// The caller is responsible for finding the segment and slicing accounts.
#[cfg(not(test))]
fn execute_forwarder_call<'info>(
    logic_ref: &crate::types::Digest,
    call: &SolanaExternalCall,
    forwarder_program_info: &anchor_lang::prelude::AccountInfo<'info>,
    cpi_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
    remaining_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
) -> Result<(), PAError> {
    let program_id = *forwarder_program_info.key;

    invoke_forwarder(
        program_id,
        &logic_ref.to_bytes(),
        &call.instruction_data,
        forwarder_program_info,
        cpi_accounts,
    )?;

    let actual_output = read_forwarder_output(&call.output_mode, &program_id, remaining_accounts)?;

    verify_output(&call.expected_output, &actual_output, &call.output_mode)?;

    anchor_lang::prelude::emit!(crate::ForwarderCallExecutedEvent {
        forwarder: program_id,
        input: call.instruction_data.clone(),
        output: actual_output,
    });

    Ok(())
}

// =============================================================================
// Main Execution
// =============================================================================

/// Execute all external calls from a transaction via CPI.
#[cfg(not(test))]
pub fn execute_external_calls<'info>(
    tx: &crate::types::Transaction,
    remaining_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
    nullifier_count: usize,
) -> Result<(), PAError> {
    use crate::settle::extract_external_calls;

    let calls = extract_external_calls(tx)?;

    if remaining_accounts.len() < nullifier_count {
        return Err(PAError::InvalidTransactionData);
    }
    let external_accounts = &remaining_accounts[nullifier_count..];

    let call_programs: Vec<Pubkey> = calls
        .iter()
        .map(|(_, call)| Pubkey::new_from_array(call.program_id))
        .collect();

    let mut cursor = 0usize;
    for (logic_ref, call) in calls {
        let program_id = Pubkey::new_from_array(call.program_id);
        let (seg_start, seg_end) =
            find_forwarder_segment(external_accounts, cursor, &program_id, &call_programs)?;

        let forwarder_program_info = &external_accounts[seg_start];
        let cpi_accounts = &external_accounts[(seg_start + 1)..seg_end];

        execute_forwarder_call(
            &logic_ref,
            &call,
            forwarder_program_info,
            cpi_accounts,
            remaining_accounts,
        )?;

        cursor = seg_end;
    }

    Ok(())
}

