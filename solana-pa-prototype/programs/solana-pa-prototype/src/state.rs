//! On-chain account definitions.
//! Named "state" to avoid conflict with Anchor's internal "accounts" module.

use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, MAX_TREE_DEPTH};
use anchor_lang::prelude::*;
use arm_core::merkle_path::PADDING_LEAF;
use arm_core::Digest;

/// The layout number of `PAStateAccount` this binary reads and writes.
/// Bumped on every change to the account layout; unrelated to release names.
#[constant]
pub const SCHEMA_VERSION: u8 = 2;

/// The schema version `migrate_state` migrates from.
#[constant]
pub const PREVIOUS_SCHEMA_VERSION: u8 = 1;

/// The commitment of the empty kind table, under which every resource
/// kind is derived via hash-to-curve: the table every deployment starts
/// on, as pa-evm's `_EMPTY_KIND_TABLE_COMMITMENT`.
#[constant]
pub const EMPTY_KIND_TABLE_COMMITMENT: [u8; 32] =
    hex_literal::hex!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");

/// Protocol Adapter state — commitment tree frontier with variable depth (1-32).
/// Nullifiers and historical roots are stored as separate PDA marker accounts.
#[account]
#[derive(InitSpace)]
pub struct PAStateAccount {
    /// Layout number of this account's bytes. Always the first field, so it
    /// sits at byte 8 of the account data (after Anchor's discriminator) in
    /// every layout: a later binary that changes the layout reads this byte
    /// through an unchecked account to decide whether it may migrate. Every
    /// instruction that reads this account — all but `initialize`, which
    /// creates it, `migrate_state`, which requires the previous version, and
    /// the development-only `dev_set_schema_version` — refuses an account
    /// whose version is not `SCHEMA_VERSION`. A layout change bumps the
    /// constant and may add, remove or reorder the other fields:
    /// `migrate_state` parses the previous layout explicitly.
    pub schema_version: u8,
    pub bump: u8,
    /// Verifier router program ID, set at initialization.
    /// Mirrors pa-evm's immutable `RISC_ZERO_VERIFIER_ROUTER`.
    pub verifier_router: Pubkey,
    /// Expected proof selector (4 bytes), set at initialization.
    /// Validated before sending proofs to the verifier router.
    pub proof_selector: [u8; 4],
    /// Kind-table commitment every settled aggregation instance must carry:
    /// the empty table's at initialization, replaced by
    /// `set_kind_table_commitment`. The aggregation circuit binds each transaction
    /// to one kind table; this pin decides which table this deployment
    /// accepts (sha256 of the concatenated entries; the empty table hashes
    /// to sha256 of zero bytes).
    pub kind_table_commitment: [u8; 32],
    /// Whether settlement is paused, as pa-evm's `paused()`: `pause` sets it
    /// and `unpause` clears it, both owner-only.
    pub paused: bool,
    /// Cached tree root (updated on every append). Avoids recomputing from frontier.
    pub root: [u8; 32],
    pub next_index: u64,
    /// Tree depth (1-32). Capacity = 2^current_depth.
    pub current_depth: u8,
    /// Filled subtree hashes at each level. Grows via realloc when tree expands.
    #[max_len(MAX_TREE_DEPTH)]
    pub frontier: Vec<[u8; 32]>,
    pub min_expiry_slots: u64,
    pub max_expiry_slots: u64,
    /// Logic refs the authority denied (`deny_logic_ref`), as pa-evm's
    /// logic-ref denylist: no settlement consumes or creates a resource
    /// carrying one. Only ever grows; the account grows by one entry per
    /// denial.
    #[max_len(0)]
    pub denied_logic_refs: Vec<[u8; 32]>,
}

impl PAStateAccount {
    /// The account at full depth, every frontier level filled, with no
    /// denied logic ref.
    pub const MAX_SPACE: usize = Self::DISCRIMINATOR.len() + Self::INIT_SPACE;

    /// The account at `depth` holding `denied` denied logic refs: full size
    /// less the frontier levels not yet reached, plus the denylist. Every
    /// other field has a fixed size, so the account grows only with the
    /// frontier and the denylist.
    pub const fn space(depth: usize, denied: usize) -> usize {
        Self::MAX_SPACE - size_of::<[u8; 32]>() * (MAX_TREE_DEPTH - depth)
            + size_of::<[u8; 32]>() * denied
    }

    pub fn is_logic_ref_denied(&self, logic_ref: &[u8; 32]) -> bool {
        self.denied_logic_refs.contains(logic_ref)
    }

    pub const INITIAL_SPACE: usize = Self::space(INITIAL_TREE_DEPTH, 0);

    pub fn depth(&self) -> usize {
        self.current_depth as usize
    }

    pub fn root_digest(&self) -> Digest {
        Digest::from_bytes(self.root)
    }

    pub fn capacity(&self) -> u64 {
        1u64 << self.current_depth
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
        self.frontier[level] = digest.into();
    }

    /// A running adapter with an empty commitment tree on the empty kind
    /// table: the state `initialize` writes.
    pub fn running(bump: u8, verifier_router: Pubkey, proof_selector: [u8; 4]) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            bump,
            verifier_router,
            proof_selector,
            kind_table_commitment: EMPTY_KIND_TABLE_COMMITMENT,
            paused: false,
            root: EMPTY_TREE_ROOT_INITIAL.into(),
            next_index: 0,
            current_depth: INITIAL_TREE_DEPTH as u8,
            frontier: vec![PADDING_LEAF.into()],
            min_expiry_slots: MIN_EXPIRY_SLOTS,
            max_expiry_slots: MAX_EXPIRY_SLOTS,
            denied_logic_refs: Vec::new(),
        }
    }
}

/// The state account in schema version 1 (`PREVIOUS_SCHEMA_VERSION`), which
/// `migrate_state` reads: this layout without the denylist. Deserialized from
/// the account's bytes after the discriminator; the bytes past its end are
/// not part of it (a shorter re-serialization leaves earlier bytes there).
#[derive(AnchorDeserialize)]
pub struct PreviousPAState {
    pub schema_version: u8,
    pub bump: u8,
    pub authority: Pubkey,
    pub verifier_router: Pubkey,
    pub proof_selector: [u8; 4],
    pub kind_table_commitment: [u8; 32],
    pub pending_authority: Option<Pubkey>,
    /// The previous build's lifecycle byte, 0 running and 1 stopped: the
    /// encoding of `paused`.
    pub paused: bool,
    pub root: [u8; 32],
    pub next_index: u64,
    pub current_depth: u8,
    pub frontier: Vec<[u8; 32]>,
    pub min_expiry_slots: u64,
    pub max_expiry_slots: u64,
}

impl From<PreviousPAState> for PAStateAccount {
    /// This layout with the previous fields, less its authority and pending
    /// authority (this layout's owner is the program's upgrade authority, as
    /// pa-evm's owner is the one who authorizes upgrades), and an empty
    /// denylist.
    fn from(previous: PreviousPAState) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            bump: previous.bump,
            verifier_router: previous.verifier_router,
            proof_selector: previous.proof_selector,
            kind_table_commitment: previous.kind_table_commitment,
            paused: previous.paused,
            root: previous.root,
            next_index: previous.next_index,
            current_depth: previous.current_depth,
            frontier: previous.frontier,
            min_expiry_slots: previous.min_expiry_slots,
            max_expiry_slots: previous.max_expiry_slots,
            denied_logic_refs: Vec::new(),
        }
    }
}

#[constant]
pub const PA_STATE_SEED: &[u8] = b"pa_state";

/// This program's ProgramData account, derived at compile time, where the
/// loader records the upgrade authority. That authority is the adapter's
/// owner, as pa-evm's owner is the one who authorizes its upgrades: it signs
/// every owner-only instruction, and moving or renouncing it
/// (`solana program set-upgrade-authority`, `--final`) moves or renounces
/// the ownership.
pub const PROGRAM_DATA: Pubkey = Pubkey::new_from_array(
    anchor_lang::derive_program_address(
        &[&crate::ID_CONST.to_bytes()],
        &anchor_lang::solana_program::bpf_loader_upgradeable::ID.to_bytes(),
    )
    .0,
);

/// Chunked transaction upload buffer.
#[account]
#[derive(InitSpace)]
pub struct TxDataAccount {
    pub bump: u8,
    pub authority: Pubkey,
    /// Account to receive rent refund on close.
    pub refund: Pubkey,
    pub written_len: u32,
    pub expires_slot: u64,
    /// Sized per upload by `space`; `INIT_SPACE` counts it empty.
    #[max_len(0)]
    pub payload: Vec<u8>,
}

impl TxDataAccount {
    /// The account holding a payload of `payload_capacity` bytes.
    pub const fn space(payload_capacity: usize) -> usize {
        Self::DISCRIMINATOR.len() + Self::INIT_SPACE + payload_capacity
    }

    /// Write a chunk at the given offset, updating `written_len` high-water mark.
    pub fn write_chunk(
        &mut self,
        offset: u32,
        data: &[u8],
    ) -> std::result::Result<(), crate::error::PAError> {
        let end = offset as usize + data.len();
        if end > self.payload.len() {
            return Err(crate::error::PAError::TxDataBoundsExceeded);
        }
        self.payload[offset as usize..end].copy_from_slice(data);
        self.written_len = std::cmp::max(self.written_len, end as u32);
        Ok(())
    }
}

#[constant]
pub const TX_DATA_SEED: &[u8] = b"tx_data";

/// Hard floor for `min_expiry_slots` configuration (~4 seconds at 400ms/slot).
#[constant]
pub const MIN_ALLOWED_EXPIRY: u64 = 10;
/// Default minimum expiry (~40 seconds at 400ms/slot).
#[constant]
pub const MIN_EXPIRY_SLOTS: u64 = 100;
/// Default maximum expiry (~24 hours at 400ms/slot).
#[constant]
pub const MAX_EXPIRY_SLOTS: u64 = 216_000;
/// Hard ceiling for `max_expiry_slots` configuration (~7 days at 400ms/slot).
#[constant]
pub const SEVEN_DAYS_SLOTS: u64 = 7 * 24 * 60 * 60 * 1000 / 400;

/// Anoma protocol deletion criterion value meaning "never delete."
/// Payloads with this criterion are emitted as on-chain events for permanent indexing.
pub const DELETION_CRITERION_NEVER: u32 = 1;

const _: () = assert!(MIN_EXPIRY_SLOTS > 0);
const _: () = assert!(MIN_EXPIRY_SLOTS < MAX_EXPIRY_SLOTS);
