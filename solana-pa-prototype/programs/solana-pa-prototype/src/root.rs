//! Root marker PDA-based storage.
//!
//! Each historical commitment tree root is stored as a separate PDA marker account.
//! Existence of the PDA indicates the root is valid for transaction construction.
//!
//! PDA derivation: `[b"root", pa_state.key(), root_bytes]`
//!
//! This enables parallel transaction construction: transactions can be built
//! against any historical root, not just the current one.

use crate::error::PAError;
use crate::merkle::PADDING_LEAF;
use crate::state::PAStateAccount;
use anchor_lang::prelude::*;
use arm_core::Digest;

/// Seeds prefix for root marker PDA derivation.
pub const ROOT_SEED: &[u8] = b"root";

/// Derive the PDA address for a root marker.
pub fn derive_root_pda(
    program_id: &Pubkey,
    pa_state: &Pubkey,
    root_bytes: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ROOT_SEED, pa_state.as_ref(), root_bytes], program_id)
}

/// Create a root marker PDA if it doesn't already exist.
///
/// Returns Ok(()) if marker was created or already exists (idempotent).
///
/// # Errors
/// * `PAError::RootPdaMismatch` - Provided marker doesn't match expected PDA
pub fn create_root_marker<'info>(
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    root_bytes: &[u8; 32],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    lamports: u64,
) -> Result<()> {
    let (expected_key, bump) = derive_root_pda(program_id, pa_state_key, root_bytes);

    // Verify the provided account matches expected PDA
    require_keys_eq!(expected_key, *marker.key, PAError::RootPdaMismatch);

    // Already ours: the root was recorded by an earlier settlement. Task 6
    // replaces this with an error; today it stays idempotent.
    if marker.owner == program_id {
        return Ok(());
    }

    let signer_seeds: &[&[u8]] = &[
        ROOT_SEED,
        pa_state_key.as_ref(),
        root_bytes.as_ref(),
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

/// Check if a root is valid for transaction construction.
///
/// A root is valid if it matches the current root, equals PADDING_LEAF
/// (ephemeral resources), or has a root marker PDA in remaining_accounts.
pub fn is_root_valid(
    state: &PAStateAccount,
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    root: &Digest,
    remaining_accounts: &[AccountInfo],
) -> bool {
    // Current root is always valid
    if root.as_bytes() == state.root {
        return true;
    }

    // Ephemeral resources anchored to initial root (PADDING_LEAF)
    if *root == PADDING_LEAF {
        return true;
    }

    // Check for root marker PDA in remaining_accounts
    let (expected_pda, _bump) = derive_root_pda(program_id, pa_state_key, &root.to_bytes());

    remaining_accounts
        .iter()
        .any(|acc| acc.key == &expected_pda && acc.owner == program_id)
}
