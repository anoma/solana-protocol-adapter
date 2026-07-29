//! Nullifier PDA-based storage.
//!
//! Each nullifier is stored as a separate PDA marker account.
//! Existence of the PDA indicates the nullifier has been spent.
//!
//! PDA derivation: `[b"nullifier", pa_state.key(), nullifier_bytes]`

use crate::error::PAError;
use anchor_lang::prelude::*;

/// Seeds prefix for nullifier PDA derivation.
pub const NULLIFIER_SEED: &[u8] = b"nullifier";

/// Derive the PDA address for a nullifier marker.
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

/// Check that a nullifier has not been spent, then create its marker PDA,
/// adopting a System-owned empty placeholder at that address if one already
/// exists (see `create_or_adopt_marker`).
///
/// This function performs an atomic check-and-create-or-adopt operation:
/// 1. Verify the provided marker account matches the expected PDA
/// 2. Check the marker isn't already owned by the program (would mean duplicate)
/// 3. Create the marker PDA via CPI to system program, or adopt a placeholder
///    already funded there
///
/// # Errors
/// * `PAError::NullifierPdaMismatch` - Provided marker doesn't match expected PDA
/// * `PAError::DuplicateNullifier` - Marker already exists (nullifier spent)
/// * `PAError::MarkerUnexpectedOwner` - Placeholder at the PDA is owned by
///   something other than the system program
/// * `PAError::MarkerUnexpectedData` - Placeholder at the PDA holds data
pub fn check_and_create_nullifier_marker<'info>(
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    nullifier_bytes: &[u8; 32],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    lamports: u64,
) -> Result<()> {
    let (expected_key, bump) = derive_nullifier_pda(program_id, pa_state_key, nullifier_bytes);

    // Verify the provided account matches expected PDA
    require_keys_eq!(expected_key, *marker.key, PAError::NullifierPdaMismatch);

    // Already ours: this nullifier was consumed by an earlier settlement.
    if marker.owner == program_id {
        return err!(PAError::DuplicateNullifier);
    }

    let signer_seeds: &[&[u8]] = &[
        NULLIFIER_SEED,
        pa_state_key.as_ref(),
        nullifier_bytes.as_ref(),
        &[bump],
    ];

    crate::marker::create_or_adopt_marker(
        program_id,
        signer_seeds,
        payer,
        marker,
        system_program,
        lamports,
    )
}
