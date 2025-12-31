//! Root marker PDA-based storage.
//!
//! Each historical commitment tree root is stored as a separate PDA marker account.
//! Existence of the PDA indicates the root is valid for transaction construction.
//!
//! PDA derivation: `[b"root", pa_state.key(), root_bytes]`
//!
//! This enables parallel transaction construction: transactions can be built
//! against any historical root, not just the current one.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke_signed, system_instruction};
use crate::types::Digest;
use crate::merkle::PADDING_LEAF;
use crate::state::PAStateAccount;
use crate::error::PAError;

/// Seeds prefix for root marker PDA derivation.
pub const ROOT_MARKER_SEED: &[u8] = b"root";

/// Derive the PDA address for a root marker.
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `pa_state` - The PA state account pubkey (included in PDA seeds for scoping)
/// * `root_bytes` - The 32-byte root digest
///
/// # Returns
/// Tuple of (PDA pubkey, bump seed)
pub fn derive_root_marker_pda(
    program_id: &Pubkey,
    pa_state: &Pubkey,
    root_bytes: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ROOT_MARKER_SEED, pa_state.as_ref(), root_bytes],
        program_id,
    )
}

/// Create a root marker PDA if it doesn't already exist.
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `pa_state_key` - The PA state account pubkey (included in PDA seeds)
/// * `root_bytes` - The 32-byte root digest
/// * `payer` - Account paying for PDA creation rent
/// * `marker` - The root marker account (must match derived PDA)
/// * `system_program` - System program for CPI
///
/// # Returns
/// Ok(()) if marker was created or already exists
pub fn create_root_marker<'info>(
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    root_bytes: &[u8; 32],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
) -> Result<()> {
    let (expected_key, bump) = derive_root_marker_pda(program_id, pa_state_key, root_bytes);

    // Verify the provided account matches expected PDA
    require_keys_eq!(expected_key, *marker.key, PAError::RootPdaMismatch);

    // If already owned by this program, marker already exists - success (idempotent)
    if marker.owner == program_id {
        return Ok(());
    }

    // Create the marker PDA with 0 data bytes (existence = valid root)
    let lamports = Rent::get()?.minimum_balance(0).max(1);

    let ix = system_instruction::create_account(
        payer.key,
        marker.key,
        lamports,
        0, // 0 bytes - existence alone indicates valid
        program_id,
    );

    let signer_seeds: &[&[u8]] = &[
        ROOT_MARKER_SEED,
        pa_state_key.as_ref(),
        root_bytes.as_ref(),
        &[bump],
    ];

    invoke_signed(
        &ix,
        &[payer.clone(), marker.clone(), system_program.clone()],
        &[signer_seeds],
    )?;

    Ok(())
}

/// Check if a root is valid for transaction construction.
///
/// A root is valid if:
/// 1. It matches the current root in PAStateAccount, OR
/// 2. It equals PADDING_LEAF (ephemeral resources), OR
/// 3. A root marker PDA exists for it in remaining_accounts
///
/// # Arguments
/// * `state` - The PA state account
/// * `pa_state_key` - The PA state account pubkey
/// * `root` - The root to check
/// * `remaining_accounts` - Accounts that may contain root marker PDAs
///
/// # Returns
/// `true` if the root is valid
pub fn is_root_valid(
    state: &PAStateAccount,
    pa_state_key: &Pubkey,
    root: &Digest,
    remaining_accounts: &[AccountInfo],
) -> bool {
    // Current root is always valid
    if root.to_bytes() == state.root {
        return true;
    }

    // Ephemeral resources anchored to initial root (PADDING_LEAF)
    if *root == PADDING_LEAF {
        return true;
    }

    // Check for root marker PDA in remaining_accounts
    let (expected_pda, _bump) = Pubkey::find_program_address(
        &[ROOT_MARKER_SEED, pa_state_key.as_ref(), &root.to_bytes()],
        &crate::ID,
    );

    remaining_accounts.iter().any(|acc| {
        acc.key == &expected_pda && acc.owner == &crate::ID
    })
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merkle::TREE_DEPTH;

    // =========================================================================
    // ROOT VALIDATION TESTS
    // =========================================================================

    #[test]
    fn test_is_root_valid_current_root() {
        let state = PAStateAccount {
            bump: 0,
            authority: Pubkey::new_unique(),
            paused: false,
            root: [0xAA; 32],
            next_index: 0,
            frontier: [[0; 32]; TREE_DEPTH],
        };
        let pa_state_key = Pubkey::new_unique();
        let root = Digest::from_bytes([0xAA; 32]);

        assert!(is_root_valid(&state, &pa_state_key, &root, &[]));
    }

    #[test]
    fn test_is_root_valid_padding_leaf() {
        let state = PAStateAccount {
            bump: 0,
            authority: Pubkey::new_unique(),
            paused: false,
            root: [0xBB; 32],
            next_index: 0,
            frontier: [[0; 32]; TREE_DEPTH],
        };
        let pa_state_key = Pubkey::new_unique();

        assert!(is_root_valid(&state, &pa_state_key, &PADDING_LEAF, &[]));
    }

    #[test]
    fn test_is_root_valid_non_current_without_marker() {
        let state = PAStateAccount {
            bump: 0,
            authority: Pubkey::new_unique(),
            paused: false,
            root: [0xAA; 32],
            next_index: 0,
            frontier: [[0; 32]; TREE_DEPTH],
        };
        let pa_state_key = Pubkey::new_unique();
        let old_root = Digest::from_bytes([0xBB; 32]);

        assert!(!is_root_valid(&state, &pa_state_key, &old_root, &[]));
    }

    // =========================================================================
    // PDA DERIVATION TESTS
    // =========================================================================

    #[test]
    fn test_derive_root_marker_pda_deterministic() {
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let root_bytes = [0xAA; 32];

        let (pda1, bump1) = derive_root_marker_pda(&program_id, &pa_state, &root_bytes);
        let (pda2, bump2) = derive_root_marker_pda(&program_id, &pa_state, &root_bytes);

        assert_eq!(pda1, pda2, "PDA derivation should be deterministic");
        assert_eq!(bump1, bump2, "Bump should be deterministic");
    }

    #[test]
    fn test_derive_root_marker_pda_different_roots_different_pdas() {
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let root1 = [0xAA; 32];
        let root2 = [0xBB; 32];

        let (pda1, _) = derive_root_marker_pda(&program_id, &pa_state, &root1);
        let (pda2, _) = derive_root_marker_pda(&program_id, &pa_state, &root2);

        assert_ne!(pda1, pda2, "Different roots should have different PDAs");
    }

    #[test]
    fn test_derive_root_marker_pda_different_pa_states_different_pdas() {
        let program_id = Pubkey::new_unique();
        let pa_state1 = Pubkey::new_unique();
        let pa_state2 = Pubkey::new_unique();
        let root = [0xAA; 32];

        let (pda1, _) = derive_root_marker_pda(&program_id, &pa_state1, &root);
        let (pda2, _) = derive_root_marker_pda(&program_id, &pa_state2, &root);

        assert_ne!(pda1, pda2, "Same root under different PA states should have different PDAs");
    }

    #[test]
    fn test_derive_root_marker_pda_off_curve() {
        let program_id = Pubkey::new_unique();
        let pa_state = Pubkey::new_unique();
        let root = [0x11; 32];

        let (pda, bump) = derive_root_marker_pda(&program_id, &pa_state, &root);

        let recreated = Pubkey::create_program_address(
            &[ROOT_MARKER_SEED, pa_state.as_ref(), &root, &[bump]],
            &program_id,
        );
        assert!(recreated.is_ok(), "PDA should be recreatable with bump");
        assert_eq!(pda, recreated.unwrap(), "Recreated PDA should match");
    }

    #[test]
    fn test_root_marker_seed_is_correct() {
        assert_eq!(ROOT_MARKER_SEED, b"root");
    }
}
