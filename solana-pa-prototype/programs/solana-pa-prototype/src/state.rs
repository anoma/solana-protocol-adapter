//! On-chain account definitions.
//! Named "state" to avoid conflict with Anchor's internal "accounts" module.

use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, MAX_TREE_DEPTH};
use anchor_lang::prelude::*;
use arm_core::merkle_path::PADDING_LEAF;
use arm_core::Digest;

/// PA lifecycle: Running → Stopped (one-way, irreversible).
///
/// Serialized as a single byte (0=Running, 1=Stopped).
/// Manual AnchorSerialize/AnchorDeserialize impl avoids the borsh 0.10/1.x
/// ambiguity that Anchor 0.31's derive macros trigger.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum PALifecycle {
    Running = 0,
    Stopped = 1,
}

impl anchor_lang::AnchorSerialize for PALifecycle {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&[*self as u8])
    }
}

impl anchor_lang::AnchorDeserialize for PALifecycle {
    fn deserialize_reader<R: std::io::Read>(reader: &mut R) -> std::io::Result<Self> {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        match byte[0] {
            0 => Ok(PALifecycle::Running),
            1 => Ok(PALifecycle::Stopped),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid PALifecycle variant",
            )),
        }
    }
}

#[cfg(feature = "idl-build")]
impl anchor_lang::IdlBuild for PALifecycle {
    fn create_type() -> Option<anchor_lang::idl::types::IdlTypeDef> {
        Some(anchor_lang::idl::types::IdlTypeDef {
            name: "PALifecycle".into(),
            docs: vec!["PA lifecycle: Running (0) or Stopped (1).".into()],
            serialization: anchor_lang::idl::types::IdlSerialization::default(),
            repr: None,
            generics: vec![],
            ty: anchor_lang::idl::types::IdlTypeDefTy::Enum {
                variants: vec![
                    anchor_lang::idl::types::IdlEnumVariant {
                        name: "Running".into(),
                        fields: None,
                    },
                    anchor_lang::idl::types::IdlEnumVariant {
                        name: "Stopped".into(),
                        fields: None,
                    },
                ],
            },
        })
    }
}

/// Protocol Adapter state — commitment tree frontier with variable depth (1-32).
/// Nullifiers and historical roots are stored as separate PDA marker accounts.
#[account]
pub struct PAStateAccount {
    /// Layout number of this account's bytes. Always the first field, so it
    /// sits at byte 8 of the account data (after Anchor's discriminator) in
    /// every layout: a later binary that changes the layout reads this byte
    /// through an unchecked account to decide whether it may migrate. Every
    /// instruction that reads this account — all but `initialize`, which
    /// creates it, and the development-only `dev_set_schema_version` —
    /// refuses an account whose version is not `SCHEMA_VERSION`. Layout
    /// changes append fields and bump the constant; they never reorder or
    /// remove fields ahead of the frontier.
    pub schema_version: u8,
    pub bump: u8,
    /// Authority that can call emergency_stop.
    pub authority: Pubkey,
    /// Verifier router program ID, set at initialization.
    /// Mirrors EVM's immutable `_TRUSTED_RISC_ZERO_VERIFIER_ROUTER`.
    pub verifier_router: Pubkey,
    /// Expected proof selector (4 bytes), set at initialization.
    /// Validated before sending proofs to the verifier router.
    pub proof_selector: [u8; 4],
    /// Kind-table commitment every settled aggregation instance must carry,
    /// set at initialization. The aggregation circuit binds each transaction
    /// to one kind table; this pin decides which table this deployment
    /// accepts (sha256 of the concatenated entries; the empty table hashes
    /// to sha256 of zero bytes).
    pub kind_table_commitment: [u8; 32],
    /// Pending authority for two-step transfer (propose + accept).
    pub pending_authority: Option<Pubkey>,
    /// Lifecycle state. One-way transition: Running → Stopped.
    pub lifecycle: PALifecycle,
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
    /// The layout this binary reads and writes. Bumped on every change to the
    /// account layout; unrelated to release names.
    pub const SCHEMA_VERSION: u8 = 1;

    /// discriminator(8) + schema_version(1) + bump(1) + authority(32) +
    /// verifier_router(32) + proof_selector(4) + kind_table_commitment(32) +
    /// pending_authority(1+32) + lifecycle(1) + root(32) + next_index(8) +
    /// current_depth(1) + min_expiry_slots(8) + max_expiry_slots(8)
    pub const BASE_SPACE: usize = 8 + 1 + 1 + 32 + 32 + 4 + 32 + 33 + 1 + 32 + 8 + 1 + 8 + 8;

    pub const VEC_OVERHEAD: usize = 4;

    pub const fn space_for_depth(depth: usize) -> usize {
        Self::BASE_SPACE + Self::VEC_OVERHEAD + (32 * depth)
    }

    pub const INITIAL_SPACE: usize = Self::space_for_depth(1);
    pub const MAX_SPACE: usize = Self::BASE_SPACE + Self::VEC_OVERHEAD + (32 * MAX_TREE_DEPTH);

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

    /// A running adapter with an empty commitment tree: the state
    /// `initialize` writes.
    pub fn running(
        bump: u8,
        authority: Pubkey,
        verifier_router: Pubkey,
        proof_selector: [u8; 4],
        kind_table_commitment: [u8; 32],
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            bump,
            authority,
            verifier_router,
            proof_selector,
            kind_table_commitment,
            pending_authority: None,
            lifecycle: PALifecycle::Running,
            root: EMPTY_TREE_ROOT_INITIAL.into(),
            next_index: 0,
            current_depth: INITIAL_TREE_DEPTH as u8,
            frontier: vec![PADDING_LEAF.into()],
            min_expiry_slots: MIN_EXPIRY_SLOTS,
            max_expiry_slots: MAX_EXPIRY_SLOTS,
        }
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

const _: () = assert!(MIN_EXPIRY_SLOTS > 0);
const _: () = assert!(MIN_EXPIRY_SLOTS < MAX_EXPIRY_SLOTS);
