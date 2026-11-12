//! CPI execution for external calls. Excluded from test builds.

use anchor_lang::error::ErrorCode;
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
) -> anchor_lang::Result<()> {
    let ix_data = build_forwarder_instruction_data(logic_ref, instruction_data);

    let ix_accounts: Vec<AccountMeta> = super::build_account_metas(segment);

    let ix = Instruction {
        program_id: *segment[0].key,
        accounts: ix_accounts,
        data: ix_data,
    };

    Ok(invoke(&ix, segment)?)
}

fn read_forwarder_output(output_mode: &OutputMode, forwarder: &Pubkey) -> Result<Vec<u8>, PAError> {
    match output_mode {
        OutputMode::ReturnData => super::decode_forwarder_output(get_return_data(), forwarder),
    }
}

/// Solana analog to EVM's `_executeForwarderCall`: invoke, verify output,
/// return the event the caller emits.
fn execute_forwarder_call(
    logic_ref: &arm_core::Digest,
    call: SolanaExternalCall,
    segment: &[AccountInfo<'_>],
) -> anchor_lang::Result<crate::ForwarderCallExecuted> {
    invoke_forwarder(&(*logic_ref).into(), &call.instruction_data, segment)?;

    let forwarder = *segment[0].key;
    let actual_output = read_forwarder_output(&call.output_mode, &forwarder)?;

    super::verify_output(&call.expected_output, &actual_output)?;

    Ok(crate::ForwarderCallExecuted {
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
    ) -> anchor_lang::Result<Self> {
        let accounts = remaining_accounts
            .get(nullifier_count..)
            .ok_or(ErrorCode::AccountNotEnoughKeys)?;
        Ok(Self { accounts })
    }

    /// Solana analog to EVM's `_executeForwarderCalls`: run a resource's
    /// external calls, each through the next segment, whose first account
    /// must be the forwarder the call names. As `_executeForwarderCall`, each
    /// call is decoded when it runs and its event emitted before the next.
    pub fn execute(
        &mut self,
        logic_ref: &arm_core::Digest,
        app_data: &arm_core::logic_instance::AppData,
        events: &crate::events::EventCpi,
    ) -> anchor_lang::Result<()> {
        for blob in &app_data.external_payload {
            let call = super::decode_external_call(blob)?;
            let seg_len = call.num_accounts as usize;
            if seg_len == 0 {
                return Err(PAError::InvalidTransactionData.into());
            }
            if seg_len > self.accounts.len() {
                return Err(ErrorCode::AccountNotEnoughKeys.into());
            }
            let (segment, rest) = self.accounts.split_at(seg_len);
            if segment[0].key != &Pubkey::new_from_array(call.program_id) {
                return Err(PAError::UnregisteredForwarder.into());
            }
            events.emit(&execute_forwarder_call(logic_ref, call, segment)?)?;
            self.accounts = rest;
        }
        Ok(())
    }
}
