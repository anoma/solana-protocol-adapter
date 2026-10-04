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
//! | 0x03      | Logs `input[1]` lines of 100 bytes, returns `Ok`  | events survive log truncation |
//! | 0x04      | Writes `input[1..]` to `remaining_accounts[0]` and returns it | writable segment accounts, output of a state change |
//!
//! Empty input is treated as mode 0x00. Mode 0x03 is called directly, as an
//! instruction of its own: it fills a transaction's 10,000-byte program-log
//! budget before a settlement in the same transaction.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::{invoke, set_return_data};
use anchor_lang::InstructionData;

// The address comes from env/<cluster>.env, which the build scripts export.
declare_id!(Pubkey::from_str_const(env!("TEST_FORWARDER_PROGRAM_ID")));

/// Mode bytes encoded in `instruction_data[0]` by fixture-gen.
pub const MODE_FAIL: u8 = 0x00;
pub const MODE_SILENT: u8 = 0x01;
pub const MODE_RELAY: u8 = 0x02;
/// Called directly by the integration suite, which reads it from the IDL.
#[constant]
pub const MODE_LOG: u8 = 0x03;
/// Writes the rest of the input to the start of the first remaining account,
/// which this program owns, and returns it.
pub const MODE_WRITE: u8 = 0x04;
/// Return data of a relay whose inner call succeeded.
pub const RELAY_OK: u8 = 0x2a;

#[program]
pub mod test_forwarder {
    use super::*;

    /// Matches the PA's `forward_call` discriminator.
    pub fn forward_call<'info>(
        ctx: Context<'info, ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let mode = input.first().copied().unwrap_or(MODE_FAIL);

        match mode {
            MODE_SILENT => Ok(()),
            MODE_RELAY => relay(ctx.remaining_accounts, &input[1..]),
            MODE_LOG => {
                let lines = input.get(1).ok_or(ErrorCode::LogLineCountMissing)?;
                let line = "x".repeat(100);
                for _ in 0..*lines {
                    msg!("{}", line);
                }
                Ok(())
            }
            MODE_WRITE => write(ctx.remaining_accounts, &input[1..]),
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
    let (logic_ref, input) = payload
        .split_first_chunk::<32>()
        .ok_or(ErrorCode::RelayPayloadTooShort)?;
    let ix = Instruction {
        program_id: *target.key,
        accounts: forwarded
            .iter()
            .map(|a| AccountMeta {
                pubkey: *a.key,
                is_signer: false,
                is_writable: a.is_writable,
            })
            .collect(),
        data: crate::instruction::ForwardCall {
            _logic_ref: *logic_ref,
            input: input.to_vec(),
        }
        .data(),
    };
    invoke(&ix, accounts)?;
    set_return_data(&[RELAY_OK]);
    Ok(())
}

/// Write `bytes` to the start of `accounts[0]`'s data and return them: a
/// forwarder that changes state, through an account its segment passes
/// writable, and reports what it did.
fn write(accounts: &[AccountInfo], bytes: &[u8]) -> Result<()> {
    let target = accounts.first().ok_or(ErrorCode::WriteTargetMissing)?;
    let mut data = target.try_borrow_mut_data()?;
    data.get_mut(..bytes.len())
        .ok_or(ErrorCode::WriteTargetTooSmall)?
        .copy_from_slice(bytes);
    set_return_data(bytes);
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
    #[msg("Log mode needs the number of lines to log")]
    LogLineCountMissing,
    #[msg("Write mode needs the account to write as its first account")]
    WriteTargetMissing,
    #[msg("Write mode's account is smaller than the bytes to write")]
    WriteTargetTooSmall,
}
