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
use crate::types::Digest;
use anchor_lang::prelude::*;
use anchor_lang::solana_program::program::invoke_signed;
use solana_system_interface::instruction as system_instruction;

/// Seeds prefix for root marker PDA derivation.
pub const ROOT_SEED: &[u8] = b"root";

/// Derive the PDA address for a root marker.
///
/// # Arguments
/// * `program_id` - The PA program ID
/// * `pa_state` - The PA state account pubkey (included in PDA seeds for scoping)
/// * `root_bytes` - The 32-byte root digest
///
/// # Returns
/// Tuple of (PDA pubkey, bump seed)
pub fn derive_root_pda(
    program_id: &Pubkey,
    pa_state: &Pubkey,
    root_bytes: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ROOT_SEED, pa_state.as_ref(), root_bytes],
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
/// Ok(()) if marker was created or already exists (idempotent)
///
/// # Errors
/// * `PAError::RootPdaMismatch` - Provided marker doesn't match expected PDA
/// * CPI errors from `invoke_signed` if account creation fails
pub fn create_root_marker<'info>(
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    root_bytes: &[u8; 32],
    payer: &AccountInfo<'info>,
    marker: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
) -> Result<()> {
    let (expected_key, bump) = derive_root_pda(program_id, pa_state_key, root_bytes);

    // Verify the provided account matches expected PDA
    require_keys_eq!(expected_key, *marker.key, PAError::RootPdaMismatch);

    // If already owned by this program, marker already exists - success (idempotent)
    if marker.owner == program_id {
        return Ok(());
    }

    // Create the marker PDA with 0 data bytes (existence = valid root).
    // NOTE: `minimum_balance(0)` returns the rent-exempt minimum for 0 data bytes.
    // Floor at 1 lamport because a 0-lamport account can be garbage-collected.
    let lamports = Rent::get()?.minimum_balance(0).max(1);

    let ix = system_instruction::create_account(
        payer.key, marker.key, lamports, 0, // 0 bytes - existence alone indicates valid
        program_id,
    );

    let signer_seeds: &[&[u8]] = &[
        ROOT_SEED,
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
/// * `program_id` - The PA program ID (for PDA derivation)
/// * `pa_state_key` - The PA state account pubkey
/// * `root` - The root to check
/// * `remaining_accounts` - Accounts that may contain root marker PDAs
///
/// # Returns
/// `true` if the root is valid
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
    let (expected_pda, _bump) =
        derive_root_pda(program_id, pa_state_key, &root.to_bytes());

    remaining_accounts
        .iter()
        .any(|acc| acc.key == &expected_pda && acc.owner == program_id)
}
