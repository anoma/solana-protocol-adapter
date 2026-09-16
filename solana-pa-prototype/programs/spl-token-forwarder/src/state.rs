//! Accounts, PDA derivation, and the wire formats of the forwarded calls.
//! Amounts are u64: SPL Token's own width.

use anchor_lang::prelude::*;
use protocol_adapter::state::{PALifecycle, PAStateAccount, PA_STATE_SEED};

/// Configuration account for the forwarder.
///
/// Mirrors EVM's ForwarderBase + EmergencyMigratableForwarderBase state.
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
    pub bump: u8,
}

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

pub const CONFIG_SEED: &[u8] = b"config";
/// Escrow authority PDA, one per token mint.
pub const ESCROW_SEED: &[u8] = b"escrow";
/// Nonce bitmap PDA, one per user per 256-nonce word.
pub const NONCE_BITMAP_SEED: &[u8] = b"nonce_bitmap";
pub const NONCES_PER_WORD: u64 = 256;

/// One 256-nonce word of a user's wrap nonces (Permit2's bitmap pattern).
///
/// Seeds: ["nonce_bitmap", user, word_index_le_bytes]. Created by anyone
/// through `init_nonce_bitmap`, which pays its rent; a wrap only reads and
/// sets bits, so no signer reaches the forwarder through the adapter's CPI.
#[account]
#[derive(InitSpace, Default)]
pub struct NonceBitmap {
    pub bits: [u8; 32],
}

impl NonceBitmap {
    /// Account size: Anchor discriminator plus the word.
    pub const ACCOUNT_SIZE: usize = 8 + Self::INIT_SPACE;

    pub fn is_used(&self, bit_position: u8) -> bool {
        let byte_index = (bit_position / 8) as usize;
        let bit_offset = bit_position % 8;
        (self.bits[byte_index] & (1 << bit_offset)) != 0
    }

    pub fn mark_used(&mut self, bit_position: u8) {
        let byte_index = (bit_position / 8) as usize;
        let bit_offset = bit_position % 8;
        self.bits[byte_index] |= 1 << bit_offset;
    }
}

/// The escrow authority for a token mint: the PDA that owns the escrow token account.
pub fn derive_escrow_pda(program_id: &Pubkey, token_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ESCROW_SEED, token_mint.as_ref()], program_id)
}

pub fn derive_nonce_bitmap_pda(
    program_id: &Pubkey,
    user: &Pubkey,
    word_index: u64,
) -> (Pubkey, u8) {
    let word_bytes = word_index.to_le_bytes();
    Pubkey::find_program_address(&[NONCE_BITMAP_SEED, user.as_ref(), &word_bytes], program_id)
}

/// The bitmap word a nonce lives in and its bit within that word.
#[inline]
pub fn nonce_to_word_and_bit(nonce: u64) -> (u64, u8) {
    (nonce / NONCES_PER_WORD, (nonce % NONCES_PER_WORD) as u8)
}

/// Length of the ed25519-signed message: base64 of a 32-byte hash.
pub const SIGNED_MESSAGE_LEN: usize = 44;

/// Standard base64 of a 32-byte hash. Wallets reject raw binary in
/// signMessage as a possible transaction, so the user signs text.
pub fn base64_of_hash(hash: &[u8; 32]) -> [u8; SIGNED_MESSAGE_LEN] {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = [b'='; SIGNED_MESSAGE_LEN];
    for (chunk, slot) in hash.chunks(3).zip(out.chunks_mut(4)) {
        let mut triple = 0u32;
        for (i, byte) in chunk.iter().enumerate() {
            triple |= (*byte as u32) << (16 - 8 * i);
        }
        for (i, c) in slot.iter_mut().enumerate().take(chunk.len() + 1) {
            *c = ALPHABET[((triple >> (18 - 6 * i)) & 0x3F) as usize];
        }
    }
    out
}

/// What the user authorizes: 120 bytes, Borsh-serialized in field order
/// (forwarder_id, token_mint, amount u64 LE, nonce u64 LE, deadline i64 LE,
/// action_tree_root). The forwarder id is the domain separator, like
/// EIP-712's verifyingContract.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct WrapMessage {
    pub forwarder_id: [u8; 32],
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
}

impl WrapMessage {
    pub const SIZE: usize = 120;

    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_vec()
            .expect("serializing fixed-size fields into memory cannot fail")
    }

    pub fn hash(&self) -> [u8; 32] {
        anchor_lang::solana_program::hash::hash(&self.to_bytes()).to_bytes()
    }

    /// The bytes the ed25519 instruction must carry: base64 of the hash.
    pub fn signed_message(&self) -> [u8; SIGNED_MESSAGE_LEN] {
        base64_of_hash(&self.hash())
    }
}

/// Wrap operand of `forward_call`, after the op-code byte: 185 bytes,
/// Borsh-serialized in field order (token_mint, amount u64 LE, user,
/// nonce u64 LE, deadline i64 LE, action_tree_root, signature,
/// ed25519_ix_index).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct WrapInput {
    pub token_mint: Pubkey,
    pub amount: u64,
    pub user: Pubkey,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
    pub signature: [u8; 64],
    /// Index of the ed25519 instruction in the transaction.
    pub ed25519_ix_index: u8,
}

impl WrapInput {
    pub const SIZE: usize = 185;

    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(crate::ErrorCode::InvalidWrapInputLength.into());
        }
        Self::try_from_slice(data).map_err(|_| crate::ErrorCode::InvalidWrapInputLength.into())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_vec()
            .expect("serializing fixed-size fields into memory cannot fail")
    }

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

/// Unwrap operand of `forward_call` after the op-code byte, and the whole
/// operand of `forward_emergency_call`: 72 bytes, Borsh-serialized in field
/// order (token_mint, amount u64 LE, recipient).
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct UnwrapInput {
    pub token_mint: Pubkey,
    pub amount: u64,
    pub recipient: Pubkey,
}

impl UnwrapInput {
    pub const SIZE: usize = 72;

    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(crate::ErrorCode::InvalidUnwrapInputLength.into());
        }
        Self::try_from_slice(data).map_err(|_| crate::ErrorCode::InvalidUnwrapInputLength.into())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.try_to_vec()
            .expect("serializing fixed-size fields into memory cannot fail")
    }
}
