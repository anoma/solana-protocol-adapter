//! State definitions and PDA derivation helpers.
//!
//! # EVM vs Solana Type Differences
//!
//! ## Amount Type: u128 (EVM) vs u64 (Solana)
//!
//! The EVM ERC20Forwarder uses `uint128` for token amounts, while this Solana
//! implementation uses `u64`. This is a platform constraint, not a design choice:
//!
//! - **SPL Token standard uses u64**: All SPL token balances and amounts are u64.
//!   This applies to both SPL Token and Token-2022 programs.
//!
//! - **Maximum representable value**: u64 max = ~18.4 quintillion atomic units.
//!   For a token with 9 decimals (like SOL), this is ~18.4 billion tokens.
//!   For 6 decimals (like USDC), this is ~18.4 trillion tokens.
//!
//! - **Practical impact**: None for production use. The largest circulating
//!   token supply on Solana is far below u64 max.
//!
//! - **Cross-chain consideration**: When bridging from EVM chains where amounts
//!   could theoretically exceed u64, the bridge must validate and truncate.
//!   This is standard practice for EVM-to-Solana bridges.

use anchor_lang::prelude::*;
use protocol_adapter::state::{PALifecycle, PAStateAccount, PA_STATE_SEED};

/// Configuration account for the forwarder.
///
/// Mirrors EVM's ForwarderBase + EmergencyMigratableForwarderBase state:
/// - protocol_adapter: Only this address can call forward_call
/// - logic_ref: Only handle this resource type
/// - emergency_committee: Can set emergency_caller when PA stopped
/// - emergency_caller: Can call forward_emergency_call when PA stopped
#[account]
#[derive(InitSpace)]
pub struct Config {
    /// The Protocol Adapter program ID that can call forward_call
    pub protocol_adapter: Pubkey,

    /// The logic reference (verifying key) this forwarder handles
    pub logic_ref: [u8; 32],

    /// Emergency committee that can set emergency_caller
    pub emergency_committee: Pubkey,

    /// Emergency caller set by committee (zero = not set)
    pub emergency_caller: Pubkey,

    /// PDA bump seed
    pub bump: u8,
}

// =============================================================================
// PA State Reading (for emergency stopped check)
// =============================================================================

/// Whether the Protocol Adapter is emergency stopped, read from its state
/// account through the adapter's own account type. Data that is not a PA
/// state account (wrong discriminator, truncated, unknown layout) is an
/// error, never "not stopped".
pub fn pa_is_stopped(pa_state_data: &[u8]) -> Result<bool> {
    let state = PAStateAccount::try_deserialize(&mut &pa_state_data[..])
        .map_err(|_| crate::ErrorCode::InvalidPaState)?;
    Ok(state.lifecycle == PALifecycle::Stopped)
}

/// Derive the PA state PDA address from the PA program ID.
pub fn derive_pa_state_pda(pa_program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PA_STATE_SEED], pa_program_id)
}

/// Seeds for config PDA
pub const CONFIG_SEED: &[u8] = b"config";

/// Seeds for escrow PDA (per token mint)
pub const ESCROW_SEED: &[u8] = b"escrow";

/// Seeds for nonce bitmap PDA (per user per word)
/// Mirrors EVM Permit2's bitmap pattern for O(1) storage per user.
pub const NONCE_BITMAP_SEED: &[u8] = b"nonce_bitmap";

/// Number of nonces per bitmap word (256 bits = 32 bytes)
pub const NONCES_PER_WORD: u64 = 256;

/// Size of a nonce bitmap in bytes (256 bits)
pub const NONCE_BITMAP_SIZE: usize = 32;

/// Derive the config PDA address.
pub fn derive_config_pda(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CONFIG_SEED], program_id)
}

/// Derive the escrow PDA address for a token mint.
///
/// The escrow PDA is the authority for the escrow token account.
/// Seeds: ["escrow", token_mint]
pub fn derive_escrow_pda(program_id: &Pubkey, token_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ESCROW_SEED, token_mint.as_ref()], program_id)
}

/// Derive the nonce bitmap PDA address for replay protection.
///
/// Mirrors EVM Permit2's bitmap pattern:
/// - Each PDA stores 256 nonces as a 32-byte bitmap
/// - Word index = nonce / 256
/// - Bit index = nonce % 256
///
/// Seeds: ["nonce_bitmap", user, word_index_le_bytes]
pub fn derive_nonce_bitmap_pda(
    program_id: &Pubkey,
    user: &Pubkey,
    word_index: u64,
) -> (Pubkey, u8) {
    let word_bytes = word_index.to_le_bytes();
    Pubkey::find_program_address(&[NONCE_BITMAP_SEED, user.as_ref(), &word_bytes], program_id)
}

/// Calculate the word index and bit position for a nonce.
///
/// Returns (word_index, bit_position) where:
/// - word_index: which 256-nonce word this nonce belongs to
/// - bit_position: which bit within that word (0-255)
#[inline]
pub fn nonce_to_word_and_bit(nonce: u64) -> (u64, u8) {
    let word_index = nonce / NONCES_PER_WORD;
    let bit_position = (nonce % NONCES_PER_WORD) as u8;
    (word_index, bit_position)
}

/// Check if a nonce bit is set in a bitmap.
///
/// # Arguments
/// * `bitmap` - 32-byte bitmap data
/// * `bit_position` - bit position (0-255)
///
/// # Returns
/// `true` if the nonce has been used, `false` otherwise.
#[inline]
pub fn is_nonce_used(bitmap: &[u8], bit_position: u8) -> bool {
    if bitmap.len() < NONCE_BITMAP_SIZE {
        return false;
    }
    let byte_index = (bit_position / 8) as usize;
    let bit_offset = bit_position % 8;
    (bitmap[byte_index] & (1 << bit_offset)) != 0
}

/// Set a nonce bit in a bitmap.
///
/// # Arguments
/// * `bitmap` - mutable 32-byte bitmap data
/// * `bit_position` - bit position (0-255)
#[inline]
pub fn set_nonce_used(bitmap: &mut [u8], bit_position: u8) {
    if bitmap.len() < NONCE_BITMAP_SIZE {
        return;
    }
    let byte_index = (bit_position / 8) as usize;
    let bit_offset = bit_position % 8;
    bitmap[byte_index] |= 1 << bit_offset;
}

// =============================================================================
// Data Structures for Input Parsing
// =============================================================================

/// Message format that user signs for wrap authorization.
///
/// User signs SHA-256 hash of this structure to authorize a specific wrap.
///
/// Includes forwarder_id for domain separation (mirrors EIP-712 domain separator):
/// - Prevents signature replay across different forwarder deployments
/// - Each forwarder deployment has a unique program ID
#[derive(Clone, Debug)]
pub struct WrapMessage {
    /// Forwarder program ID for domain separation (like EIP-712 verifyingContract)
    pub forwarder_id: [u8; 32],
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
}

impl WrapMessage {
    // Byte offsets for serialization (prevents off-by-one errors on struct changes)
    const OFF_FORWARDER_ID: usize = 0;
    const OFF_TOKEN_MINT: usize = 32; // forwarder_id (32)
    const OFF_AMOUNT: usize = 64; // + token_mint (32)
    const OFF_NONCE: usize = 72; // + amount (8)
    const OFF_DEADLINE: usize = 80; // + nonce (8)
    const OFF_ACTION_ROOT: usize = 88; // + deadline (8)
    /// Total serialized size in bytes.
    pub const SIZE: usize = 120; // + action_tree_root (32)

    /// Serialize to bytes for hashing.
    ///
    /// Layout (120 bytes):
    /// | Offset | Size | Field            |
    /// |--------|------|------------------|
    /// | 0      | 32   | forwarder_id     |
    /// | 32     | 32   | token_mint       |
    /// | 64     | 8    | amount (u64 LE)  |
    /// | 72     | 8    | nonce (u64 LE)   |
    /// | 80     | 8    | deadline (i64 LE)|
    /// | 88     | 32   | action_tree_root |
    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0u8; Self::SIZE];
        bytes[Self::OFF_FORWARDER_ID..Self::OFF_TOKEN_MINT].copy_from_slice(&self.forwarder_id);
        bytes[Self::OFF_TOKEN_MINT..Self::OFF_AMOUNT].copy_from_slice(&self.token_mint);
        bytes[Self::OFF_AMOUNT..Self::OFF_NONCE].copy_from_slice(&self.amount.to_le_bytes());
        bytes[Self::OFF_NONCE..Self::OFF_DEADLINE].copy_from_slice(&self.nonce.to_le_bytes());
        bytes[Self::OFF_DEADLINE..Self::OFF_ACTION_ROOT]
            .copy_from_slice(&self.deadline.to_le_bytes());
        bytes[Self::OFF_ACTION_ROOT..Self::SIZE].copy_from_slice(&self.action_tree_root);
        bytes
    }

    /// Compute SHA-256 hash of the message (what gets signed).
    pub fn hash(&self) -> [u8; 32] {
        use anchor_lang::solana_program::hash::hashv;
        hashv(&[&self.to_bytes()]).to_bytes()
    }
}

/// Input structure for wrap operation (parsed from forward_call input).
///
/// Note: EVM uses uint128 for amounts; Solana uses u64 (see module docs).
#[derive(Clone, Debug)]
pub struct WrapInput {
    pub token_mint: Pubkey,
    /// Amount of tokens to wrap (u64 per SPL Token; EVM uses uint128)
    pub amount: u64,
    pub user: Pubkey,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
    pub signature: [u8; 64],
    pub ed25519_ix_index: u8,
}

impl WrapInput {
    // Byte offsets for parsing (prevents off-by-one errors on struct changes)
    const OFF_TOKEN_MINT: usize = 0;
    const OFF_AMOUNT: usize = 32; // token_mint (32)
    const OFF_USER: usize = 40; // + amount (8)
    const OFF_NONCE: usize = 72; // + user (32)
    const OFF_DEADLINE: usize = 80; // + nonce (8)
    const OFF_ACTION_ROOT: usize = 88; // + deadline (8)
    const OFF_SIGNATURE: usize = 120; // + action_tree_root (32)
    const OFF_IX_INDEX: usize = 184; // + signature (64)
    /// Total expected input size in bytes.
    pub const SIZE: usize = 185; // + ed25519_ix_index (1)

    /// Parse from bytes (excluding op code byte).
    ///
    /// Layout (185 bytes):
    /// | Offset | Size | Field            |
    /// |--------|------|------------------|
    /// | 0      | 32   | token_mint       |
    /// | 32     | 8    | amount (u64 LE)  |
    /// | 40     | 32   | user             |
    /// | 72     | 8    | nonce (u64 LE)   |
    /// | 80     | 8    | deadline (i64 LE)|
    /// | 88     | 32   | action_tree_root |
    /// | 120    | 64   | signature        |
    /// | 184    | 1    | ed25519_ix_index |
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(crate::ErrorCode::InvalidWrapInputLength.into());
        }

        let token_mint = Pubkey::new_from_array(
            data[Self::OFF_TOKEN_MINT..Self::OFF_AMOUNT]
                .try_into()
                .unwrap(),
        );
        let amount = u64::from_le_bytes(data[Self::OFF_AMOUNT..Self::OFF_USER].try_into().unwrap());
        let user =
            Pubkey::new_from_array(data[Self::OFF_USER..Self::OFF_NONCE].try_into().unwrap());
        let nonce = u64::from_le_bytes(
            data[Self::OFF_NONCE..Self::OFF_DEADLINE]
                .try_into()
                .unwrap(),
        );
        let deadline = i64::from_le_bytes(
            data[Self::OFF_DEADLINE..Self::OFF_ACTION_ROOT]
                .try_into()
                .unwrap(),
        );
        let action_tree_root: [u8; 32] = data[Self::OFF_ACTION_ROOT..Self::OFF_SIGNATURE]
            .try_into()
            .unwrap();
        let signature: [u8; 64] = data[Self::OFF_SIGNATURE..Self::OFF_IX_INDEX]
            .try_into()
            .unwrap();
        let ed25519_ix_index = data[Self::OFF_IX_INDEX];

        Ok(Self {
            token_mint,
            amount,
            user,
            nonce,
            deadline,
            action_tree_root,
            signature,
            ed25519_ix_index,
        })
    }

    /// Convert to WrapMessage for hashing/verification.
    ///
    /// # Arguments
    /// * `forwarder_id` - The forwarder program ID for domain separation
    pub fn to_message(&self, forwarder_id: &Pubkey) -> WrapMessage {
        WrapMessage {
            forwarder_id: forwarder_id.to_bytes(),
            token_mint: self.token_mint.to_bytes(),
            amount: self.amount,
            nonce: self.nonce,
            deadline: self.deadline,
            action_tree_root: self.action_tree_root,
        }
    }
}

/// Input structure for unwrap operation.
///
/// Note: EVM uses uint128 for amounts; Solana uses u64 (see module docs).
#[derive(Clone, Debug)]
pub struct UnwrapInput {
    pub token_mint: Pubkey,
    /// Amount of tokens to unwrap (u64 per SPL Token; EVM uses uint128)
    pub amount: u64,
    pub recipient: Pubkey,
}

impl UnwrapInput {
    // Byte offsets for parsing (prevents off-by-one errors on struct changes)
    const OFF_TOKEN_MINT: usize = 0;
    const OFF_AMOUNT: usize = 32; // token_mint (32)
    const OFF_RECIPIENT: usize = 40; // + amount (8)
    /// Total expected input size in bytes.
    pub const SIZE: usize = 72; // + recipient (32)

    /// Parse from bytes (excluding op code byte).
    ///
    /// Layout (72 bytes):
    /// | Offset | Size | Field           |
    /// |--------|------|-----------------|
    /// | 0      | 32   | token_mint      |
    /// | 32     | 8    | amount (u64 LE) |
    /// | 40     | 32   | recipient       |
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(crate::ErrorCode::InvalidUnwrapInputLength.into());
        }

        let token_mint = Pubkey::new_from_array(
            data[Self::OFF_TOKEN_MINT..Self::OFF_AMOUNT]
                .try_into()
                .unwrap(),
        );
        let amount = u64::from_le_bytes(
            data[Self::OFF_AMOUNT..Self::OFF_RECIPIENT]
                .try_into()
                .unwrap(),
        );
        let recipient =
            Pubkey::new_from_array(data[Self::OFF_RECIPIENT..Self::SIZE].try_into().unwrap());

        Ok(Self {
            token_mint,
            amount,
            recipient,
        })
    }
}
