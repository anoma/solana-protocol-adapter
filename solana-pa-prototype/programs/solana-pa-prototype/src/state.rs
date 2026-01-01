//! Anchor account definitions for on-chain PAState.
//! Named "state" to avoid conflict with Anchor's internal "accounts" module.

use anchor_lang::prelude::*;
use crate::merkle::TREE_DEPTH;
use crate::types::Digest;

/// On-chain Protocol Adapter state.
/// Stores the commitment tree frontier only.
/// Nullifiers are stored as separate PDA marker accounts (see nullifier.rs).
#[account]
pub struct PAStateAccount {
    /// Bump seed for PDA derivation.
    pub bump: u8,

    /// Authority that can call emergency_stop.
    pub authority: Pubkey,

    /// Whether the protocol is paused (one-way, requires upgrade to unpause).
    pub paused: bool,

    /// Current tree root (computed from frontier).
    /// Stored for quick access without recomputation.
    pub root: [u8; 32],

    /// Next leaf index in the commitment tree.
    pub next_index: u64,

    /// Commitment tree frontier (filled subtree hashes at each level).
    /// Length = TREE_DEPTH (32). Each element is a 32-byte digest.
    pub frontier: [[u8; 32]; TREE_DEPTH],

    /// Minimum slots in the future for TxData expiration.
    /// Default: MIN_EXPIRY_SLOTS (100). Zero means use compile-time constant.
    pub min_expiry_slots: u64,

    /// Maximum slots in the future for TxData expiration.
    /// Default: MAX_EXPIRY_SLOTS (216_000). Zero means use compile-time constant.
    pub max_expiry_slots: u64,
}

impl PAStateAccount {
    /// Calculate space required for this account.
    /// discriminator (8) + bump (1) + authority (32) + paused (1) + root (32) + next_index (8)
    /// + frontier (32 * 32) + min_expiry_slots (8) + max_expiry_slots (8)
    pub const SPACE: usize = 8 + 1 + 32 + 1 + 32 + 8 + (32 * TREE_DEPTH) + 8 + 8;

    /// Get the current root as a Digest.
    pub fn get_root(&self) -> Digest {
        Digest::from_bytes(self.root)
    }

    /// Get frontier entry at level as a Digest.
    pub fn get_frontier(&self, level: usize) -> Digest {
        Digest::from_bytes(self.frontier[level])
    }

    /// Set frontier entry at level.
    pub fn set_frontier(&mut self, level: usize, digest: Digest) {
        self.frontier[level] = digest.to_bytes();
    }

    /// Set root.
    pub fn set_root(&mut self, digest: Digest) {
        self.root = digest.to_bytes();
    }

    /// Get min_expiry_slots with fallback for migration (0 = use compile-time constant).
    pub fn get_min_expiry_slots(&self) -> u64 {
        if self.min_expiry_slots == 0 {
            MIN_EXPIRY_SLOTS
        } else {
            self.min_expiry_slots
        }
    }

    /// Get max_expiry_slots with fallback for migration (0 = use compile-time constant).
    pub fn get_max_expiry_slots(&self) -> u64 {
        if self.max_expiry_slots == 0 {
            MAX_EXPIRY_SLOTS
        } else {
            self.max_expiry_slots
        }
    }
}

/// Seeds for PAState PDA derivation.
pub const PA_STATE_SEED: &[u8] = b"pa_state";

/// TxData account for chunked transaction upload.
#[account]
pub struct TxDataAccount {
    /// Bump seed for PDA derivation.
    pub bump: u8,

    /// Authority that can write.
    pub authority: Pubkey,

    /// Account to receive rent refund on close.
    pub refund: Pubkey,

    /// Bytes written so far.
    pub written_len: u32,

    /// Slot when this account expires.
    pub expires_slot: u64,

    /// Payload data (variable length, allocated at creation).
    pub payload: Vec<u8>,
}

impl TxDataAccount {
    /// Header size (excluding payload).
    /// discriminator (8) + bump (1) + authority (32) + refund (32) + written_len (4) + expires_slot (8) = 85
    pub const HEADER_SIZE: usize = 8 + 1 + 32 + 32 + 4 + 8;

    /// Calculate space required for a TxData account with given payload capacity.
    pub fn space(payload_capacity: usize) -> usize {
        Self::HEADER_SIZE + 4 + payload_capacity // +4 for Vec length prefix
    }
}

/// Seeds for TxData PDA derivation.
pub const TX_DATA_SEED: &[u8] = b"tx_data";

/// Minimum slots in the future for TxData expiration (~40 seconds at 400ms/slot).
pub const MIN_EXPIRY_SLOTS: u64 = 100;

/// Maximum slots in the future for TxData expiration (~24 hours at 400ms/slot).
pub const MAX_EXPIRY_SLOTS: u64 = 216_000;
