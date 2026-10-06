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
                get_return_data().ok_or(PAError::ForwarderCallOutputMismatch)?;
            if returned_program_id != *program_id {
                return Err(PAError::ForwarderCallOutputMismatch);
            }
            Ok(return_data)
        }
    }
}

/// Solana analog to EVM's `_executeForwarderCall`: invoke, verify output,
/// return the event the caller emits.
fn execute_forwarder_call(
    logic_ref: &arm_core::Digest,
    call: SolanaExternalCall,
    segment: &[AccountInfo<'_>],
) -> Result<crate::ForwarderCallExecutedEvent, PAError> {
    invoke_forwarder(&(*logic_ref).into(), &call.instruction_data, segment)?;

    let forwarder = *segment[0].key;
    let actual_output = read_forwarder_output(&call.output_mode, &forwarder)?;

    super::verify_output(&call.expected_output, &actual_output)?;

    Ok(crate::ForwarderCallExecutedEvent {
        untrusted_forwarder: forwarder,
        input: call.instruction_data,
        output: actual_output,
    })
}

/// The forwarder account segments of `remaining_accounts` after the
/// nullifier markers, taken in call order as each resource's calls run.
pub struct ForwarderSegments<'a, 'info> {
    accounts: &'a [AccountInfo<'info>],
}

impl<'a, 'info> ForwarderSegments<'a, 'info> {
    pub fn new(
        remaining_accounts: &'a [AccountInfo<'info>],
        nullifier_count: usize,
    ) -> Result<Self, PAError> {
        let accounts = remaining_accounts
            .get(nullifier_count..)
            .ok_or(PAError::InvalidTransactionData)?;
        Ok(Self { accounts })
    }

    /// Solana analog to EVM's `_executeForwarderCalls`: run a resource's
    /// external calls, each through the next segment, whose first account
    /// must be the forwarder the call names. Returns one event per call.
    pub fn execute(
        &mut self,
        logic_ref: &arm_core::Digest,
        app_data: &arm_core::logic_instance::AppData,
    ) -> Result<Vec<crate::ForwarderCallExecutedEvent>, PAError> {
        let calls = super::decode_external_calls(app_data)?;
        let mut events = Vec::with_capacity(calls.len());
        for call in calls {
            let seg_len = call.num_accounts as usize;
            if seg_len == 0 || seg_len > self.accounts.len() {
                return Err(PAError::InvalidTransactionData);
            }
            let (segment, rest) = self.accounts.split_at(seg_len);
            if segment[0].key != &Pubkey::new_from_array(call.program_id) {
                return Err(PAError::UnregisteredForwarder);
            }
            events.push(execute_forwarder_call(logic_ref, call, segment)?);
            self.accounts = rest;
        }
        Ok(events)
    }
}
