//! Accounts, PDA derivation, and the wire formats of the forwarded calls.
//! Amounts are u64: SPL Token's own width.

use anchor_lang::prelude::*;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
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
    /// The version the config was last initialized at, as OpenZeppelin
    /// Initializable's `_initialized`: `initialize` records CONFIG_VERSION,
    /// and `reinitialize` raises it to CONFIG_VERSION once.
    pub version: u64,
}

impl Config {
    /// The account's size: the discriminator and the fields.
    pub const ACCOUNT_SIZE: usize = Self::DISCRIMINATOR.len() + Self::INIT_SPACE;
}

/// The config version this build initializes to and reinitializes to, as
/// the `n` of an OpenZeppelin `reinitializer(n)`. A build that rotates the
/// logic ref raises it by one, so its `reinitialize` runs once.
#[constant]
pub const CONFIG_VERSION: u64 = 2;

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

#[constant]
pub const CONFIG_SEED: &[u8] = b"config";
/// The config's address, derived at compile time: `initialize` creates the
/// config only at this canonical PDA, so checking an account against it
/// costs no PDA derivation.
pub const CONFIG_PDA: Pubkey = Pubkey::new_from_array(
    anchor_lang::derive_program_address(&[CONFIG_SEED], &crate::ID_CONST.to_bytes()).0,
);
/// Seed of the escrow authority.
#[constant]
pub const ESCROW_SEED: &[u8] = b"escrow";
const ESCROW_AUTHORITY_DERIVATION: ([u8; 32], u8) =
    anchor_lang::derive_program_address(&[ESCROW_SEED], &crate::ID_CONST.to_bytes());
/// The escrow authority, derived at compile time: the one PDA that owns every
/// mint's escrow token account, as the EVM forwarder holds every token at
/// its own address. Checking an account against it costs no PDA derivation.
pub const ESCROW_AUTHORITY: Pubkey = Pubkey::new_from_array(ESCROW_AUTHORITY_DERIVATION.0);
/// The escrow authority's canonical bump, with which it signs.
pub const ESCROW_AUTHORITY_BUMP: u8 = ESCROW_AUTHORITY_DERIVATION.1;
/// The escrow authority's signer seeds, with its compile-time bump.
pub(crate) const ESCROW_SIGNER_SEEDS: &[&[u8]] = &[ESCROW_SEED, &[ESCROW_AUTHORITY_BUMP]];
/// Nonce bitmap PDA, one per user per 256-nonce word.
#[constant]
pub const NONCE_BITMAP_SEED: &[u8] = b"nonce_bitmap";
#[constant]
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
    /// The canonical bump of the bitmap's address, stored by `init_nonce_bitmap`.
    pub bump: u8,
}

impl NonceBitmap {
    /// Account size: Anchor discriminator, the word, and the bump.
    pub const ACCOUNT_SIZE: usize = Self::DISCRIMINATOR.len() + Self::INIT_SPACE;

    /// Whether `key` is the address of `user`'s word `word_index` under the
    /// stored bump. `init_nonce_bitmap` creates bitmaps only at the canonical
    /// address and stores its bump, so this recognizes exactly that bitmap
    /// without searching for the bump.
    pub fn is_at(&self, key: &Pubkey, program_id: &Pubkey, user: &Pubkey, word_index: u64) -> bool {
        Pubkey::create_program_address(
            &[
                NONCE_BITMAP_SEED,
                user.as_ref(),
                &word_index.to_le_bytes(),
                &[self.bump],
            ],
            program_id,
        )
        .is_ok_and(|address| address == *key)
    }

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

/// Size of a config the previous build created: the discriminator, the
/// four 32-byte fields, then the config's bump, which this build derives at
/// compile time (CONFIG_PDA), and no version.
#[constant]
pub const PREVIOUS_CONFIG_SIZE: u64 = (Config::DISCRIMINATOR.len() + 4 * 32 + 1) as u64;
/// Size of a nonce bitmap the previous build created: the word, without the
/// bump this build stores.
#[constant]
pub const PREVIOUS_NONCE_BITMAP_SIZE: u64 = (NonceBitmap::ACCOUNT_SIZE - 1) as u64;

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
    let mut out = [0u8; SIGNED_MESSAGE_LEN];
    STANDARD
        .encode_slice(hash, &mut out)
        .expect("32 bytes encode to exactly 44 base64 characters");
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

    /// sha256 of the Borsh encoding, hashed field by field in Borsh order.
    pub fn hash(&self) -> [u8; 32] {
        solana_sha256_hasher::hashv(&[
            &self.forwarder_id,
            &self.token_mint,
            &self.amount.to_le_bytes(),
            &self.nonce.to_le_bytes(),
            &self.deadline.to_le_bytes(),
            &self.action_tree_root,
        ])
        .to_bytes()
    }

    /// The bytes the ed25519 instruction must carry: base64 of the hash.
    pub fn signed_message(&self) -> [u8; SIGNED_MESSAGE_LEN] {
        base64_of_hash(&self.hash())
    }
}

/// Wrap operand of `forward_call`, after the op-code byte: 121 bytes,
/// Borsh-serialized in field order (token_mint, amount u64 LE, user,
/// nonce u64 LE, deadline i64 LE, action_tree_root, ed25519_ix_index).
/// The user's signature is not part of the input: it travels in the
/// ed25519 instruction `ed25519_ix_index` names, which the forwarder
/// verifies through the instructions sysvar.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug)]
pub struct WrapInput {
    pub token_mint: Pubkey,
    pub amount: u64,
    pub user: Pubkey,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
    /// Index of the ed25519 instruction in the transaction.
    pub ed25519_ix_index: u8,
}

impl WrapInput {
    pub const SIZE: usize = 121;

    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(crate::ErrorCode::InvalidWrapInputLength.into());
        }
        Self::try_from_slice(data).map_err(|_| crate::ErrorCode::InvalidWrapInputLength.into())
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
        borsh::to_vec(self).expect("serializing fixed-size fields into memory cannot fail")
    }
}
