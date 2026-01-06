//! State definitions and PDA derivation helpers.

use anchor_lang::prelude::*;

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

    /// Whether the forwarder is in emergency stopped state
    pub is_stopped: bool,

    /// PDA bump seed
    pub bump: u8,
}

/// Seeds for config PDA
pub const CONFIG_SEED: &[u8] = b"config";

/// Seeds for escrow PDA (per token mint)
pub const ESCROW_SEED: &[u8] = b"escrow";

/// Seeds for nonce PDA (per user per nonce)
pub const NONCE_SEED: &[u8] = b"nonce";

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

/// Derive the nonce PDA address for replay protection.
///
/// If this PDA exists, the nonce has been used.
/// Seeds: ["nonce", user, nonce_le_bytes]
pub fn derive_nonce_pda(program_id: &Pubkey, user: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    let nonce_bytes = nonce.to_le_bytes();
    Pubkey::find_program_address(&[NONCE_SEED, user.as_ref(), &nonce_bytes], program_id)
}

// =============================================================================
// Data Structures for Input Parsing
// =============================================================================

/// Message format that user signs for wrap authorization.
///
/// User signs SHA-256 hash of this structure to authorize a specific wrap.
#[derive(Clone, Debug)]
pub struct WrapMessage {
    pub token_mint: [u8; 32],
    pub amount: u64,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
}

impl WrapMessage {
    /// Serialize to bytes for hashing.
    pub fn to_bytes(&self) -> [u8; 88] {
        let mut bytes = [0u8; 88];
        bytes[0..32].copy_from_slice(&self.token_mint);
        bytes[32..40].copy_from_slice(&self.amount.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.nonce.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.deadline.to_le_bytes());
        bytes[56..88].copy_from_slice(&self.action_tree_root);
        bytes
    }

    /// Compute SHA-256 hash of the message (what gets signed).
    pub fn hash(&self) -> [u8; 32] {
        use anchor_lang::solana_program::hash::hashv;
        hashv(&[&self.to_bytes()]).to_bytes()
    }
}

/// Input structure for wrap operation (parsed from forward_call input).
#[derive(Clone, Debug)]
pub struct WrapInput {
    pub token_mint: Pubkey,
    pub amount: u64,
    pub user: Pubkey,
    pub nonce: u64,
    pub deadline: i64,
    pub action_tree_root: [u8; 32],
    pub signature: [u8; 64],
    pub ed25519_ix_index: u8,
}

impl WrapInput {
    /// Parse from bytes (excluding op code byte).
    ///
    /// Layout:
    /// - token_mint: 32 bytes
    /// - amount: 8 bytes (u64 LE)
    /// - user: 32 bytes
    /// - nonce: 8 bytes (u64 LE)
    /// - deadline: 8 bytes (i64 LE)
    /// - action_tree_root: 32 bytes
    /// - signature: 64 bytes
    /// - ed25519_ix_index: 1 byte
    ///
    /// Total: 185 bytes
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != 185 {
            return Err(crate::ErrorCode::InvalidInput.into());
        }

        let token_mint = Pubkey::new_from_array(data[0..32].try_into().unwrap());
        let amount = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let user = Pubkey::new_from_array(data[40..72].try_into().unwrap());
        let nonce = u64::from_le_bytes(data[72..80].try_into().unwrap());
        let deadline = i64::from_le_bytes(data[80..88].try_into().unwrap());
        let action_tree_root: [u8; 32] = data[88..120].try_into().unwrap();
        let signature: [u8; 64] = data[120..184].try_into().unwrap();
        let ed25519_ix_index = data[184];

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
    pub fn to_message(&self) -> WrapMessage {
        WrapMessage {
            token_mint: self.token_mint.to_bytes(),
            amount: self.amount,
            nonce: self.nonce,
            deadline: self.deadline,
            action_tree_root: self.action_tree_root,
        }
    }
}

/// Input structure for unwrap operation.
#[derive(Clone, Debug)]
pub struct UnwrapInput {
    pub token_mint: Pubkey,
    pub amount: u64,
    pub recipient: Pubkey,
}

impl UnwrapInput {
    /// Parse from bytes (excluding op code byte).
    ///
    /// Layout:
    /// - token_mint: 32 bytes
    /// - amount: 8 bytes (u64 LE)
    /// - recipient: 32 bytes
    ///
    /// Total: 72 bytes
    pub fn try_from_bytes(data: &[u8]) -> Result<Self> {
        if data.len() != 72 {
            return Err(crate::ErrorCode::InvalidInput.into());
        }

        let token_mint = Pubkey::new_from_array(data[0..32].try_into().unwrap());
        let amount = u64::from_le_bytes(data[32..40].try_into().unwrap());
        let recipient = Pubkey::new_from_array(data[40..72].try_into().unwrap());

        Ok(Self {
            token_mint,
            amount,
            recipient,
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_message_serialization() {
        let msg = WrapMessage {
            token_mint: [1u8; 32],
            amount: 1000,
            nonce: 42,
            deadline: 1700000000,
            action_tree_root: [2u8; 32],
        };

        let bytes = msg.to_bytes();
        assert_eq!(bytes.len(), 88);

        // Verify token_mint
        assert_eq!(&bytes[0..32], &[1u8; 32]);

        // Verify amount
        assert_eq!(
            u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            1000
        );

        // Verify nonce
        assert_eq!(
            u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
            42
        );

        // Verify deadline
        assert_eq!(
            i64::from_le_bytes(bytes[48..56].try_into().unwrap()),
            1700000000
        );

        // Verify action_tree_root
        assert_eq!(&bytes[56..88], &[2u8; 32]);
    }

    #[test]
    fn test_wrap_input_parsing() {
        let mut data = vec![0u8; 185];

        // token_mint
        data[0..32].copy_from_slice(&[1u8; 32]);
        // amount
        data[32..40].copy_from_slice(&1000u64.to_le_bytes());
        // user
        data[40..72].copy_from_slice(&[2u8; 32]);
        // nonce
        data[72..80].copy_from_slice(&42u64.to_le_bytes());
        // deadline
        data[80..88].copy_from_slice(&1700000000i64.to_le_bytes());
        // action_tree_root
        data[88..120].copy_from_slice(&[3u8; 32]);
        // signature
        data[120..184].copy_from_slice(&[4u8; 64]);
        // ed25519_ix_index
        data[184] = 0;

        let input = WrapInput::try_from_bytes(&data).unwrap();
        assert_eq!(input.token_mint.to_bytes(), [1u8; 32]);
        assert_eq!(input.amount, 1000);
        assert_eq!(input.user.to_bytes(), [2u8; 32]);
        assert_eq!(input.nonce, 42);
        assert_eq!(input.deadline, 1700000000);
        assert_eq!(input.action_tree_root, [3u8; 32]);
        assert_eq!(input.signature, [4u8; 64]);
        assert_eq!(input.ed25519_ix_index, 0);
    }

    #[test]
    fn test_unwrap_input_parsing() {
        let mut data = vec![0u8; 72];

        // token_mint
        data[0..32].copy_from_slice(&[1u8; 32]);
        // amount
        data[32..40].copy_from_slice(&500u64.to_le_bytes());
        // recipient
        data[40..72].copy_from_slice(&[2u8; 32]);

        let input = UnwrapInput::try_from_bytes(&data).unwrap();
        assert_eq!(input.token_mint.to_bytes(), [1u8; 32]);
        assert_eq!(input.amount, 500);
        assert_eq!(input.recipient.to_bytes(), [2u8; 32]);
    }

    #[test]
    fn test_pda_derivation() {
        let program_id = Pubkey::new_unique();
        let token_mint = Pubkey::new_unique();
        let user = Pubkey::new_unique();

        // Config PDA
        let (config_pda, config_bump) = derive_config_pda(&program_id);
        assert!(config_bump <= 255);
        assert_ne!(config_pda, Pubkey::default());

        // Escrow PDA
        let (escrow_pda, escrow_bump) = derive_escrow_pda(&program_id, &token_mint);
        assert!(escrow_bump <= 255);
        assert_ne!(escrow_pda, Pubkey::default());

        // Nonce PDA
        let (nonce_pda, nonce_bump) = derive_nonce_pda(&program_id, &user, 42);
        assert!(nonce_bump <= 255);
        assert_ne!(nonce_pda, Pubkey::default());

        // Different nonces should give different PDAs
        let (nonce_pda2, _) = derive_nonce_pda(&program_id, &user, 43);
        assert_ne!(nonce_pda, nonce_pda2);
    }
}
