//! Test Forwarder — Minimal forwarder program for testing PA error paths.
//!
//! Exposes a single `forward_call` instruction (matching the PA's hardcoded
//! `FORWARD_CALL_DISCRIMINATOR`) that dispatches behavior based on `input[0]`:
//!
//! | Mode byte | Behavior                                          | PA error tested             |
//! |-----------|---------------------------------------------------|-----------------------------|
//! | 0x00      | Returns `Err(IntentionalFailure)`                 | ExternalCallCpiFailed       |
//! | 0x01      | Returns `Ok` without calling `set_return_data`    | ExternalCallOutputMismatch  |
//! | 0x02      | Writes `input[1..]` to `remaining_accounts[0]`    | OutputAccount mode          |
//!
//! Empty input is treated as mode 0x00.

use anchor_lang::prelude::*;

declare_id!("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD");

/// Mode bytes encoded in `instruction_data[0]` by fixture-gen.
const MODE_FAIL: u8 = 0x00;
const MODE_SILENT: u8 = 0x01;
const MODE_WRITE_ACCOUNT: u8 = 0x02;

#[program]
pub mod test_forwarder {
    use super::*;

    /// Matches the PA's `forward_call` discriminator.
    pub fn forward_call(
        ctx: Context<ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        let mode = input.first().copied().unwrap_or(MODE_FAIL);

        match mode {
            MODE_FAIL => Err(ErrorCode::IntentionalFailure.into()),
            MODE_SILENT => Ok(()),
            MODE_WRITE_ACCOUNT => {
                let payload = &input[1..];

                require!(
                    !ctx.remaining_accounts.is_empty(),
                    ErrorCode::NoWritableAccount
                );
                let account = &ctx.remaining_accounts[0];
                require!(account.is_writable, ErrorCode::NoWritableAccount);

                let mut data = account.try_borrow_mut_data()?;
                let write_len = payload.len().min(data.len());
                data[..write_len].copy_from_slice(&payload[..write_len]);
                Ok(())
            }
            _ => Err(ErrorCode::IntentionalFailure.into()),
        }
    }
}

#[derive(Accounts)]
pub struct ForwardCallAccounts {}

#[error_code]
pub enum ErrorCode {
    #[msg("Intentional failure for testing")]
    IntentionalFailure,
    #[msg("No writable account provided")]
    NoWritableAccount,
}
