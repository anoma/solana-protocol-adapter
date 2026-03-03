//! Block Time Forwarder - Example forwarder program for time comparison.
//!
//! This is a minimal example of a Solana forwarder that mirrors the EVM
//! BlockTimeForwarder functionality. It compares an expected timestamp
//! against the current Solana clock time and returns the result.
//!
//! This demonstrates the forwarder pattern for the Protocol Adapter:
//! - PA CPIs to this forwarder with logic_ref and input
//! - Forwarder executes its logic (time comparison)
//! - Forwarder returns output via set_return_data
//! - PA reads return data and verifies against expected_output

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::set_return_data;

declare_id!("J1YYaBphwHzvGDq6EGY71DfPkuKWxMtHrtzGMUHp1LZ6");

/// Comparison result constants.
pub const RESULT_LT: u8 = 0; // expected < current
pub const RESULT_EQ: u8 = 1; // expected == current
pub const RESULT_GT: u8 = 2; // expected > current

#[program]
pub mod block_time_forwarder {
    use super::*;

    /// Forward a time comparison call.
    ///
    /// # Arguments
    /// * `logic_ref` - The resource's logic verifying key (for access control, not used in this example)
    /// * `input` - 8 bytes representing the expected unix timestamp (i64, little-endian)
    ///
    /// # Returns
    /// Sets return data to a single byte:
    /// * 0 = expected_time < current_time (LT)
    /// * 1 = expected_time == current_time (EQ)
    /// * 2 = expected_time > current_time (GT)
    pub fn forward_call(
        ctx: Context<ForwardCall>,
        logic_ref: [u8; 32],
        input: Vec<u8>,
    ) -> Result<()> {
        // Log the call for debugging
        msg!("BlockTimeForwarder: forward_call invoked");
        msg!("  logic_ref: {:?}", &logic_ref[..8]);

        // Decode expected timestamp from input (8 bytes, little-endian i64)
        if input.len() != 8 {
            msg!("  ERROR: input must be 8 bytes, got {}", input.len());
            return Err(ErrorCode::InvalidInput.into());
        }

        let expected_time =
            i64::from_le_bytes(input.try_into().map_err(|_| ErrorCode::InvalidInput)?);
        msg!("  expected_time: {}", expected_time);

        // Get current time from Solana clock sysvar
        let current_time = ctx.accounts.clock.unix_timestamp;
        msg!("  current_time: {}", current_time);

        // Compare and determine result
        let result = match expected_time.cmp(&current_time) {
            std::cmp::Ordering::Less => {
                msg!("  result: LT (expected < current)");
                RESULT_LT
            }
            std::cmp::Ordering::Greater => {
                msg!("  result: GT (expected > current)");
                RESULT_GT
            }
            std::cmp::Ordering::Equal => {
                msg!("  result: EQ (expected == current)");
                RESULT_EQ
            }
        };

        // Return result via set_return_data
        set_return_data(&[result]);
        msg!("  return_data set: [{}]", result);

        Ok(())
    }
}

#[derive(Accounts)]
pub struct ForwardCall<'info> {
    pub clock: Sysvar<'info, Clock>,
}

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid input: expected 8 bytes for timestamp")]
    InvalidInput,
}
