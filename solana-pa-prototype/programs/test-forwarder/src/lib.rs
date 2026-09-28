//! Test Forwarder — Minimal forwarder program for testing PA error paths.
//!
//! Exposes a single `forward_call` instruction (matching the PA's hardcoded
//! `FORWARD_CALL_DISCRIMINATOR`) that dispatches behavior based on `input[0]`:
//!
//! | Mode byte | Behavior                                          | PA error tested             |
//! |-----------|---------------------------------------------------|-----------------------------|
//! | 0x00      | Returns `Err(IntentionalFailure)`                 | ExternalCallCpiFailed       |
//! | 0x01      | Returns `Ok` without calling `set_return_data`    | ExternalCallOutputMismatch  |
//! | 0x02      | Relays `forward_call` to `remaining_accounts[0]`  | forwarder caller check      |
//!
//! Empty input is treated as mode 0x00.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, set_return_data};
use anchor_lang::InstructionData;

declare_id!("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD");

/// Mode bytes encoded in `instruction_data[0]` by fixture-gen.
pub const MODE_FAIL: u8 = 0x00;
pub const MODE_SILENT: u8 = 0x01;
pub const MODE_RELAY: u8 = 0x02;
/// Return data of a relay whose inner call succeeded.
pub const RELAY_OK: u8 = 0x2a;

#[program]
pub mod test_forwarder {
    use super::*;

    /// Matches the PA's `forward_call` discriminator.
    pub fn forward_call<'info>(
        ctx: Context<'_, '_, 'info, 'info, ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let mode = input.first().copied().unwrap_or(MODE_FAIL);

        match mode {
            MODE_SILENT => Ok(()),
            MODE_RELAY => relay(ctx.remaining_accounts, &input[1..]),
            _ => Err(ErrorCode::IntentionalFailure.into()),
        }
    }
}

/// Call `forward_call` on the program in `accounts[0]`, handing it the rest of
/// `accounts` without signer privileges, the logic ref `payload[..32]` and the
/// input `payload[32..]`. This is a program the adapter invokes acting as the
/// adapter toward another forwarder; the forwarder's caller check must reject it.
fn relay<'info>(accounts: &[AccountInfo<'info>], payload: &[u8]) -> Result<()> {
    let (target, forwarded) = accounts
        .split_first()
        .ok_or(ErrorCode::RelayMissingTarget)?;
    if payload.len() < 32 {
        return Err(ErrorCode::RelayPayloadTooShort.into());
    }
    let (logic_ref, input) = payload.split_at(32);
    let ix = Instruction {
        program_id: *target.key,
        accounts: forwarded
            .iter()
            .map(|a| {
                if a.is_writable {
                    AccountMeta::new(*a.key, false)
                } else {
                    AccountMeta::new_readonly(*a.key, false)
                }
            })
            .collect(),
        data: crate::instruction::ForwardCall {
            _logic_ref: logic_ref.try_into().expect("split at 32"),
            input: input.to_vec(),
        }
        .data(),
    };
    invoke(&ix, accounts)?;
    set_return_data(&[RELAY_OK]);
    Ok(())
}

#[derive(Accounts)]
pub struct ForwardCallAccounts {}

#[error_code]
pub enum ErrorCode {
    #[msg("Intentional failure for testing")]
    IntentionalFailure,
    #[msg("Relay needs the target program as its first account")]
    RelayMissingTarget,
    #[msg("Relay payload must start with a 32-byte logic ref")]
    RelayPayloadTooShort,
}
