//! CPI execution for external calls. Excluded from test builds.

use anchor_lang::prelude::{AccountInfo, Pubkey};
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{get_return_data, invoke};

use crate::error::PAError;
use crate::types::{OutputMode, SolanaExternalCall};

use super::build_forwarder_instruction_data;

/// Find the account segment for a forwarder in remaining_accounts.
///
/// Segments start with the forwarder program account, followed by any CPI accounts
/// the forwarder needs. Segments appear in the same order as external calls.
///
/// Returns (seg_start, seg_end) indices into external_accounts.
fn find_forwarder_segment(
    external_accounts: &[AccountInfo],
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
    for (i, account) in external_accounts.iter().enumerate().skip(seg_start + 1) {
        if call_programs.iter().any(|p| account.key == p) {
            seg_end = i;
            break;
        }
    }

    Ok((seg_start, seg_end))
}

/// Invoke a forwarder program via CPI.
fn invoke_forwarder<'info>(
    program_id: Pubkey,
    logic_ref: &[u8; 32],
    instruction_data: &[u8],
    forwarder_program_info: &AccountInfo<'info>,
    cpi_accounts: &[AccountInfo<'info>],
) -> Result<(), PAError> {
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

    let mut invoke_infos: Vec<AccountInfo<'info>> = Vec::with_capacity(1 + cpi_accounts.len());
    invoke_infos.push(forwarder_program_info.clone());
    invoke_infos.extend_from_slice(cpi_accounts);

    invoke(&ix, &invoke_infos)?;
    Ok(())
}

/// Read forwarder output based on the output mode.
fn read_forwarder_output(
    output_mode: &OutputMode,
    program_id: &Pubkey,
    remaining_accounts: &[AccountInfo<'_>],
) -> Result<Vec<u8>, PAError> {
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

/// Execute a single external call via CPI.
///
/// This is the Solana analog to EVM's `_executeForwarderCall`:
/// 1. Invoke forwarder via CPI
/// 2. Read and verify output
/// 3. Emit event
///
/// The caller is responsible for finding the segment and slicing accounts.
fn execute_forwarder_call<'info>(
    logic_ref: &crate::types::Digest,
    call: &SolanaExternalCall,
    forwarder_program_info: &AccountInfo<'info>,
    cpi_accounts: &[AccountInfo<'info>],
    remaining_accounts: &[AccountInfo<'info>],
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

    super::verify_output(&call.expected_output, &actual_output)?;

    anchor_lang::prelude::emit!(crate::ForwarderCallExecutedEvent {
        forwarder: program_id,
        input: call.instruction_data.clone(),
        output: actual_output,
    });

    Ok(())
}

/// Execute all external calls from a transaction via CPI.
pub fn execute_external_calls(
    tx: &crate::types::Transaction,
    remaining_accounts: &[AccountInfo<'_>],
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
