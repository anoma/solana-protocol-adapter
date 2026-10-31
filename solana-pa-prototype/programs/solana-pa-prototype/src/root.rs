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
use anchor_lang::prelude::*;
use arm_core::merkle_path::PADDING_LEAF;
use arm_core::Digest;

/// Seeds prefix for root marker PDA derivation.
#[constant]
pub const ROOT_SEED: &[u8] = b"root";

/// Derive the PDA address for a root marker. The derivation costs about
/// 1500 CU, which is why a settlement checks each root once
/// (`HistoricalRoots`).
pub fn derive_root_pda(
    program_id: &Pubkey,
    pa_state: &Pubkey,
    root_bytes: &[u8; 32],
) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ROOT_SEED, pa_state.as_ref(), root_bytes], program_id)
}

/// Create a root marker PDA for a newly produced root, adopting a
/// System-owned empty placeholder at that address if one already exists
/// (see `create_or_adopt_marker`).
///
/// # Errors
/// * `PAError::RootPdaMismatch` - Provided marker doesn't match expected PDA
/// * `PAError::PreExistingRoot` - Marker already recorded (repeated root)
/// * `PAError::MarkerUnexpectedOwner` - Placeholder at the PDA is owned by
///   something other than the system program
/// * `PAError::MarkerUnexpectedData` - Placeholder at the PDA holds data
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

    // A repeated produced root is unreachable in normal operation: a root is
    // recorded only by a settlement that appends commitments, and next_index
    // strictly increases. Reaching this means a hash collision or a
    // tree-accounting bug, both of which must surface rather than pass quietly.
    if marker.owner == program_id {
        return err!(PAError::PreExistingRoot);
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
/// A root is valid if it is `latest_root`, the tree's latest recorded root,
/// equals PADDING_LEAF (ephemeral resources), or has a root marker PDA in
/// remaining_accounts.
pub fn is_root_valid(
    latest_root: &[u8; 32],
    program_id: &Pubkey,
    pa_state_key: &Pubkey,
    root: &Digest,
    remaining_accounts: &[AccountInfo],
) -> bool {
    if root.as_bytes() == latest_root {
        return true;
    }

    // Ephemeral resources anchored to initial root (PADDING_LEAF)
    if *root == PADDING_LEAF {
        return true;
    }

    // Check for root marker PDA in remaining_accounts
    let (expected_pda, _bump) = derive_root_pda(program_id, pa_state_key, &(*root).into());

    remaining_accounts
        .iter()
        .any(|acc| acc.key == &expected_pda && acc.owner == program_id)
}

/// A settlement's check that the roots its consumed resources reference are
/// historical, against the root set as it stood before the transaction: the
/// set does not change until every action ran, as pa-evm's. Each root is
/// checked once (see `derive_root_pda`).
pub struct HistoricalRoots<'a, 'info> {
    latest_root: [u8; 32],
    pa_state_key: &'a Pubkey,
    remaining_accounts: &'a [AccountInfo<'info>],
    accepted: Vec<Digest>,
}

impl<'a, 'info> HistoricalRoots<'a, 'info> {
    /// The check against the tree whose latest root is `latest_root`, its
    /// markers among `remaining_accounts`, for a settlement consuming
    /// `nullifier_count` resources, so at most that many roots.
    pub fn new(
        latest_root: [u8; 32],
        pa_state_key: &'a Pubkey,
        remaining_accounts: &'a [AccountInfo<'info>],
        nullifier_count: usize,
    ) -> Self {
        Self {
            latest_root,
            pa_state_key,
            remaining_accounts,
            // The bump allocator never frees memory, so growing would waste
            // heap.
            accepted: Vec::with_capacity(nullifier_count),
        }
    }

    /// Requires `root` to be historical, as pa-evm's
    /// `_isCommitmentTreeRootHistorical` check.
    ///
    /// # Errors
    /// * `PAError::NonExistingRoot` - `root` is not valid (`is_root_valid`)
    pub fn require_historical(&mut self, root: &Digest) -> Result<()> {
        if !self.accepted.contains(root) {
            require!(
                is_root_valid(
                    &self.latest_root,
                    &crate::ID,
                    self.pa_state_key,
                    root,
                    self.remaining_accounts,
                ),
                PAError::NonExistingRoot
            );
            self.accepted.push(*root);
        }
        Ok(())
    }
}
