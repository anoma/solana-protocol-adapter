//! On-chain account definitions.
//! Named "state" to avoid conflict with Anchor's internal "accounts" module.

use crate::merkle::MAX_TREE_DEPTH;
use crate::types::Digest;
use anchor_lang::prelude::*;

/// Protocol Adapter state — commitment tree frontier with variable depth (1-32).
/// Nullifiers and historical roots are stored as separate PDA marker accounts.
#[account]
pub struct PAStateAccount {
    pub bump: u8,
    /// Authority that can call emergency_stop.
    pub authority: Pubkey,
    /// One-way pause; requires upgrade to unpause.
    pub paused: bool,
    /// Cached tree root (updated on every append). Avoids recomputing from frontier.
    pub root: [u8; 32],
    pub next_index: u64,
    /// Tree depth (1-32). Capacity = 2^current_depth.
    pub current_depth: u8,
    /// Filled subtree hashes at each level. Grows via realloc when tree expands.
    pub frontier: Vec<[u8; 32]>,
    pub min_expiry_slots: u64,
    pub max_expiry_slots: u64,
}

impl PAStateAccount {
    /// discriminator(8) + bump(1) + authority(32) + paused(1) + root(32) +
    /// next_index(8) + current_depth(1) + min_expiry_slots(8) + max_expiry_slots(8)
    pub const BASE_SPACE: usize = 8 + 1 + 32 + 1 + 32 + 8 + 1 + 8 + 8;

    pub const VEC_OVERHEAD: usize = 4;

    pub const fn space_for_depth(depth: usize) -> usize {
        Self::BASE_SPACE + Self::VEC_OVERHEAD + (32 * depth)
    }

    pub const INITIAL_SPACE: usize = Self::space_for_depth(1);
    pub const MAX_SPACE: usize = Self::BASE_SPACE + Self::VEC_OVERHEAD + (32 * MAX_TREE_DEPTH);

    pub fn depth(&self) -> usize {
        self.current_depth as usize
    }

    pub fn capacity(&self) -> u64 {
        1u64 << self.current_depth
    }

    pub fn needs_growth(&self) -> bool {
        self.next_index >= self.capacity()
    }

    pub fn can_grow(&self) -> bool {
        (self.current_depth as usize) < MAX_TREE_DEPTH
    }

    /// Returns the new level index. Caller must check `can_grow()` first.
    pub fn grow(&mut self) -> usize {
        debug_assert!(self.can_grow(), "caller must check can_grow()");
        let new_level = self.current_depth as usize;
        self.frontier.push([0u8; 32]); // Will be filled by caller
        self.current_depth += 1;
        new_level
    }

    pub fn get_frontier(&self, level: usize) -> Digest {
        Digest::from_bytes(self.frontier[level])
    }

    pub fn set_frontier(&mut self, level: usize, digest: Digest) {
        self.frontier[level] = digest.to_bytes();
    }
}

pub const PA_STATE_SEED: &[u8] = b"pa_state";

/// Chunked transaction upload buffer.
#[account]
pub struct TxDataAccount {
    pub bump: u8,
    pub authority: Pubkey,
    /// Account to receive rent refund on close.
    pub refund: Pubkey,
    pub written_len: u32,
    pub expires_slot: u64,
    pub payload: Vec<u8>,
}

impl TxDataAccount {
    /// discriminator(8) + bump(1) + authority(32) + refund(32) + written_len(4) + expires_slot(8)
    pub const HEADER_SIZE: usize = 8 + 1 + 32 + 32 + 4 + 8;

    pub fn space(payload_capacity: usize) -> usize {
        Self::HEADER_SIZE + 4 + payload_capacity // +4 for Vec length prefix
    }
}

pub const TX_DATA_SEED: &[u8] = b"tx_data";

/// Hard floor for `min_expiry_slots` configuration (~4 seconds at 400ms/slot).
pub const MIN_ALLOWED_EXPIRY: u64 = 10;
/// Default minimum expiry (~40 seconds at 400ms/slot).
pub const MIN_EXPIRY_SLOTS: u64 = 100;
/// Default maximum expiry (~24 hours at 400ms/slot).
pub const MAX_EXPIRY_SLOTS: u64 = 216_000;
/// Hard ceiling for `max_expiry_slots` configuration (~7 days at 400ms/slot).
pub const SEVEN_DAYS_SLOTS: u64 = 7 * 24 * 60 * 60 * 1000 / 400;

/// Anoma protocol deletion criterion value meaning "never delete."
/// Payloads with this criterion are emitted as on-chain events for permanent indexing.
pub const DELETION_CRITERION_NEVER: u32 = 1;
