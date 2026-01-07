//! Tests for state types and PDA derivation

use crate::state::{
    WrapMessage, WrapInput, UnwrapInput,
    derive_config_pda, derive_escrow_pda, derive_nonce_pda,
};
use anchor_lang::prelude::Pubkey;

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
