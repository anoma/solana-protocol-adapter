//! Nullifier PDA-based storage.
//!
//! Each nullifier is stored as a separate PDA marker account.
//! Existence of the PDA indicates the nullifier has been spent.
//!
//! PDA derivation: `[b"nullifier", pa_state.key(), nullifier_bytes]`

use crate::error::PAError;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction};

/// Seeds prefix for nullifier PDA derivation.
pub const NULLIFIER_SEED: &[u8] = b"nullifier";

/// Derive the PDA address for a nullifier marker.
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `pa_state` - The PA state account pubkey (included in PDA seeds for scoping)
/// * `nullifier_bytes` - The 32-byte nullifier digest
///
/// # Returns
/// Tuple of (PDA pubkey, bump seed)
pub fn derive_nullifier_pda(
    program_id: &Pubkey,
    pa_state: &Pubkey,
    nullifier_bytes: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[NULLIFIER_SEED, pa_state.as_ref(), nullifier_bytes],
        program_id,
    )
}

/// Check that a nullifier has not been spent, then create its marker PDA.
///
/// This function performs an atomic check-and-create operation:
/// 1. Verify the provided marker account matches the expected PDA
/// 2. Check the marker isn't already owned by the program (would mean duplicate)
/// 3. Create the marker PDA via CPI to system program
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `pa_state_key` - The PA state account pubkey (included in PDA seeds)
/// * `nullifier_bytes` - The 32-byte nullifier digest
/// * `payer` - Account paying for PDA creation rent
/// * `marker` - The nullifier marker account (must match derived PDA)
/// * `system_program` - System program for CPI
///
/// # Errors
/// * `PAError::NullifierPdaMismatch` - Provided marker doesn't match expected PDA
/// * `PAError::DuplicateNullifier` - Marker already exists (nullifier spent)
pub fn check_and_create_nullifier_marker<'info>(
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    nullifier_bytes: &[u8; 32],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
) -> Result<()> {
    let (expected_key, bump) = derive_nullifier_pda(program_id, pa_state_key, nullifier_bytes);

    // Verify the provided account matches expected PDA
    require_keys_eq!(expected_key, *marker.key, PAError::NullifierPdaMismatch);

    // If already owned by this program, nullifier was previously spent
    if marker.owner == program_id {
        return err!(PAError::DuplicateNullifier);
    }

    // Create the marker PDA with 0 data bytes (existence = spent)
    // NOTE: `minimum_balance(0)` is 0 lamports, but a 0-lamport account is
    // effectively non-existent (can be reclaimed). Ensure the marker persists.
    let lamports = Rent::get()?.minimum_balance(0).max(1);

    let ix = system_instruction::create_account(
        payer.key, marker.key, lamports, 0, // 0 bytes - existence alone indicates spent
        program_id,
    );

    let signer_seeds: &[&[u8]] = &[
        NULLIFIER_SEED,
        pa_state_key.as_ref(),
        nullifier_bytes.as_ref(),
        &[bump],
    ];

    invoke_signed(
        &ix,
        &[payer.clone(), marker.clone(), system_program.clone()],
        &[signer_seeds],
    )?;

    Ok(())
}

/// Check if a nullifier marker exists (i.e., nullifier is spent).
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `marker` - The nullifier marker account
///
/// # Returns
/// `true` if the marker exists and is owned by the program (nullifier spent)
pub fn is_nullifier_spent(program_id: &Pubkey, marker: &AccountInfo) -> bool {
    marker.owner == program_id
}
