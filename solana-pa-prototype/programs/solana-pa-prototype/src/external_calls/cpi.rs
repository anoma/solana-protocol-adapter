//! CPI execution for external calls. Excluded from test builds.

use anchor_lang::prelude::{AccountInfo, Pubkey};
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{get_return_data, invoke};

use crate::error::PAError;
use crate::types::{OutputMode, SolanaExternalCall};

use super::build_forwarder_instruction_data;

/// Invoke a forwarder program via CPI.
///
/// `segment` must start with the forwarder program account, followed by CPI accounts.
/// The segment slice is passed directly to `invoke`, avoiding a Vec allocation.
fn invoke_forwarder(
    logic_ref: &[u8; 32],
    instruction_data: &[u8],
    segment: &[AccountInfo<'_>],
) -> Result<(), PAError> {
    let ix_data = build_forwarder_instruction_data(logic_ref, instruction_data);

    let ix_accounts: Vec<AccountMeta> = super::build_account_metas(segment);

    let ix = Instruction {
        program_id: *segment[0].key,
        accounts: ix_accounts,
        data: ix_data,
    };

    invoke(&ix, segment).map_err(|e| {
        anchor_lang::prelude::msg!("CPI failed: {:?}", e);
        PAError::ExternalCallCpiFailed
    })?;
    Ok(())
}

fn read_forwarder_output(
    output_mode: &OutputMode,
    program_id: &Pubkey,
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
    }
}

/// Solana analog to EVM's `_executeForwarderCall`: invoke, verify output, emit event.
fn execute_forwarder_call<'info>(
    logic_ref: &arm_core::Digest,
    call: SolanaExternalCall,
    segment: &[AccountInfo<'info>],
    events: &crate::events::EventCpi<'info>,
) -> Result<(), PAError> {
    invoke_forwarder(&(*logic_ref).into(), &call.instruction_data, segment)?;

    let forwarder = *segment[0].key;
    let actual_output = read_forwarder_output(&call.output_mode, &forwarder)?;

    super::verify_output(&call.expected_output, &actual_output)?;

    events
        .emit(&crate::ForwarderCallExecutedEvent {
            forwarder,
            input: call.instruction_data,
            output: actual_output,
        })
        .map_err(|_| PAError::EventEmissionFailed)?;

    Ok(())
}

/// Execute all external calls from the aggregation instance via CPI.
pub fn execute_external_calls<'info>(
    instance: &arm_core::aggregation_instance::AggregationInstance,
    remaining_accounts: &[AccountInfo<'info>],
    nullifier_count: usize,
    events: &crate::events::EventCpi<'info>,
) -> Result<(), PAError> {
    let calls = super::extract_external_calls(instance)?;

    if remaining_accounts.len() < nullifier_count {
        return Err(PAError::InvalidTransactionData);
    }
    let external_accounts = &remaining_accounts[nullifier_count..];

    let mut cursor = 0usize;
    for (logic_ref, call) in calls.into_iter() {
        let seg_len = call.num_accounts as usize;
        let seg_end = cursor
            .checked_add(seg_len)
            .ok_or(PAError::InvalidTransactionData)?;
        if seg_end > external_accounts.len() {
            return Err(PAError::InvalidTransactionData);
        }

        // Validate that the segment's first account matches the expected forwarder program ID.
        if seg_len == 0 {
            return Err(PAError::InvalidTransactionData);
        }
        let expected_program = Pubkey::new_from_array(call.program_id);
        if external_accounts[cursor].key != &expected_program {
            return Err(PAError::UnregisteredForwarder);
        }

        execute_forwarder_call(
            &logic_ref,
            call,
            &external_accounts[cursor..seg_end],
            events,
        )?;

        cursor = seg_end;
    }

    Ok(())
}
