//! Test Forwarder - Minimal forwarder program for testing PA error paths.
//!
//! Provides three instruction handlers that match the `forward_call` signature
//! but exercise different failure and output modes:
//! - `forward_call_fail`: Always returns an error (tests ExternalCallCpiFailed).
//! - `forward_call_silent`: Returns Ok without setting return data (tests output mismatch).
//! - `forward_call_write_account`: Writes input to remaining_accounts[0] (tests OutputAccount mode).

use anchor_lang::prelude::*;

declare_id!("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD");

#[program]
pub mod test_forwarder {
    use super::*;

    /// Always fails with IntentionalFailure.
    /// Used to test the ExternalCallCpiFailed error path in the PA.
    pub fn forward_call_fail(
        _ctx: Context<ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        _input: Vec<u8>,
    ) -> Result<()> {
        Err(ErrorCode::IntentionalFailure.into())
    }

    /// Returns Ok without calling set_return_data.
    /// The PA expects return data from the forwarder; its absence triggers
    /// ExternalCallOutputMismatch via `get_return_data().ok_or(...)`.
    pub fn forward_call_silent(
        _ctx: Context<ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        _input: Vec<u8>,
    ) -> Result<()> {
        msg!("TestForwarder: forward_call_silent returning Ok with no return data");
        Ok(())
    }

    /// Writes the input bytes to remaining_accounts[0] starting at offset 0.
    /// The PA can then read from that account via OutputMode::OutputAccount.
    pub fn forward_call_write_account(
        ctx: Context<ForwardCallAccounts>,
        _logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        msg!("TestForwarder: forward_call_write_account writing {} bytes", input.len());

        require!(!ctx.remaining_accounts.is_empty(), ErrorCode::NoWritableAccount);

        let account = &ctx.remaining_accounts[0];
        require!(account.is_writable, ErrorCode::NoWritableAccount);

        let mut data = account.try_borrow_mut_data()?;
        let write_len = input.len().min(data.len());
        data[..write_len].copy_from_slice(&input[..write_len]);

        msg!("TestForwarder: wrote {} bytes to account", write_len);
        Ok(())
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
