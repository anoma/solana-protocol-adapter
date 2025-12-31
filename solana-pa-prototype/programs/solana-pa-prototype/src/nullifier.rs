//! Nullifier PDA-based storage.
//!
//! Each nullifier is stored as a separate PDA marker account.
//! Existence of the PDA indicates the nullifier has been spent.
//!
//! PDA derivation: `[b"nullifier", pa_state.key(), nullifier_bytes]`

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction};
use crate::error::PAError;

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
        payer.key,
        marker.key,
        lamports,
        0, // 0 bytes - existence alone indicates spent
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

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------------
    // derive_nullifier_pda tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_derive_nullifier_pda_deterministic() {
        // Same inputs should always produce the same PDA
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier_bytes = [0xAA; 32];

        let (pda1, bump1) = derive_nullifier_pda(&program_id, &pa_state, &nullifier_bytes);
        let (pda2, bump2) = derive_nullifier_pda(&program_id, &pa_state, &nullifier_bytes);

        assert_eq!(pda1, pda2, "PDA derivation should be deterministic");
        assert_eq!(bump1, bump2, "Bump should be deterministic");
    }

    #[test]
    fn test_derive_nullifier_pda_different_nullifiers_different_pdas() {
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier1 = [0xAA; 32];
        let nullifier2 = [0xBB; 32];

        let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state, &nullifier1);
        let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state, &nullifier2);

        assert_ne!(pda1, pda2, "Different nullifiers should have different PDAs");
    }

    #[test]
    fn test_derive_nullifier_pda_different_pa_states_different_pdas() {
        // Nullifiers are scoped to PA state, so same nullifier under different PA states
        // should produce different PDAs
        let program_id = Pubkey::new_unique();
        let pa_state1 = Pubkey::new_unique();
        let pa_state2 = Pubkey::new_unique();
        let nullifier = [0xAA; 32];

        let (pda1, _) = derive_nullifier_pda(&program_id, &pa_state1, &nullifier);
        let (pda2, _) = derive_nullifier_pda(&program_id, &pa_state2, &nullifier);

        assert_ne!(pda1, pda2, "Same nullifier under different PA states should have different PDAs");
    }

    #[test]
    fn test_derive_nullifier_pda_different_programs_different_pdas() {
        let program_id1 = Pubkey::new_unique();
        let program_id2 = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier = [0xAA; 32];

        let (pda1, _) = derive_nullifier_pda(&program_id1, &pa_state, &nullifier);
        let (pda2, _) = derive_nullifier_pda(&program_id2, &pa_state, &nullifier);

        assert_ne!(pda1, pda2, "Same nullifier under different programs should have different PDAs");
    }

    #[test]
    fn test_derive_nullifier_pda_off_curve() {
        // PDAs must be off the ed25519 curve
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier = [0x11; 32];

        let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

        // Verify we can recreate the PDA with the bump
        let recreated = Pubkey::create_program_address(
            &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
            &program_id,
        );
        assert!(recreated.is_ok(), "PDA should be recreatable with bump");
        assert_eq!(pda, recreated.unwrap(), "Recreated PDA should match");
    }

    #[test]
    fn test_derive_nullifier_pda_all_zeros_nullifier() {
        // Edge case: all-zeros nullifier should work
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier = [0x00; 32];

        let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

        // Should produce valid PDA
        let recreated = Pubkey::create_program_address(
            &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
            &program_id,
        );
        assert!(recreated.is_ok(), "All-zeros nullifier should produce valid PDA");
        assert_eq!(pda, recreated.unwrap());
    }

    #[test]
    fn test_derive_nullifier_pda_all_ones_nullifier() {
        // Edge case: all-ones nullifier should work
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let nullifier = [0xFF; 32];

        let (pda, bump) = derive_nullifier_pda(&program_id, &pa_state, &nullifier);

        // Should produce valid PDA
        let recreated = Pubkey::create_program_address(
            &[NULLIFIER_SEED, pa_state.as_ref(), &nullifier, &[bump]],
            &program_id,
        );
        assert!(recreated.is_ok(), "All-ones nullifier should produce valid PDA");
        assert_eq!(pda, recreated.unwrap());
    }

    // -------------------------------------------------------------------------
    // Seed constant tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_nullifier_seed_is_correct() {
        assert_eq!(NULLIFIER_SEED, b"nullifier");
    }
}
