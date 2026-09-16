//! Input parsing and wrap-message tests ported from the AnomaPay EVM
//! forwarder suite (ERC20Forwarder.t.sol). Every other behaviour of the
//! program — caller checks, emergency flow, transfers, events — needs a
//! running validator and lives in the TypeScript integration suite.

use crate::state::{UnwrapInput, WrapInput, WrapMessage};
use anchor_lang::prelude::Pubkey;

#[test]
fn test_wrap_input_wrong_length() {
    // Test input too short
    let short_input = vec![0u8; WrapInput::SIZE - 1];
    let result = WrapInput::try_from_bytes(&short_input);
    assert!(result.is_err());

    // Test input too long
    let long_input = vec![0u8; WrapInput::SIZE + 1];
    let result = WrapInput::try_from_bytes(&long_input);
    assert!(result.is_err());

    // Test correct length succeeds
    let correct_input = vec![0u8; WrapInput::SIZE];
    let result = WrapInput::try_from_bytes(&correct_input);
    assert!(result.is_ok());
}

/// Mirrors: test_unwrap_reverts_if_the_input_length_is_wrong
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_input_wrong_length() {
    // Test input too short
    let short_input = vec![0u8; UnwrapInput::SIZE - 1];
    let result = UnwrapInput::try_from_bytes(&short_input);
    assert!(result.is_err());

    // Test input too long
    let long_input = vec![0u8; UnwrapInput::SIZE + 1];
    let result = UnwrapInput::try_from_bytes(&long_input);
    assert!(result.is_err());

    // Test correct length succeeds
    let correct_input = vec![0u8; UnwrapInput::SIZE];
    let result = UnwrapInput::try_from_bytes(&correct_input);
    assert!(result.is_ok());
}

/// Test that WrapMessage serialization is deterministic for signature verification
#[test]
fn test_wrap_message_deterministic_serialization() {
    let program_id = Pubkey::new_unique();

    let msg1 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    let msg2 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    // Same input should produce same hash
    assert_eq!(msg1.hash(), msg2.hash());

    // Different input should produce different hash
    let msg3 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1001, // Different amount
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };
    assert_ne!(msg1.hash(), msg3.hash());
}

/// Test that different forwarder IDs produce different hashes (domain separation)
/// This mirrors EIP-712's verifyingContract in the domain separator
#[test]
fn test_wrap_message_domain_separation_forwarder_id() {
    let program_id_1 = Pubkey::new_unique();
    let program_id_2 = Pubkey::new_unique();

    let msg1 = WrapMessage {
        forwarder_id: program_id_1.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    let msg2 = WrapMessage {
        forwarder_id: program_id_2.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    // Different forwarder_id should produce different hash
    // This prevents replay across different forwarder deployments
    assert_ne!(msg1.hash(), msg2.hash());
}

/// Test WrapInput parsing round-trip
#[test]
fn test_wrap_input_parsing_fields() {
    let token_mint = Pubkey::new_unique();
    let user = Pubkey::new_unique();
    let amount: u64 = 1_000_000;
    let nonce: u64 = 123;
    let deadline: i64 = 1700000000;
    let action_tree_root = [0xABu8; 32];
    let signature = [0xCDu8; 64];
    let ed25519_ix_index: u8 = 0;

    let mut input = Vec::with_capacity(WrapInput::SIZE);
    input.extend_from_slice(&token_mint.to_bytes());
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&user.to_bytes());
    input.extend_from_slice(&nonce.to_le_bytes());
    input.extend_from_slice(&deadline.to_le_bytes());
    input.extend_from_slice(&action_tree_root);
    input.extend_from_slice(&signature);
    input.push(ed25519_ix_index);

    let parsed = WrapInput::try_from_bytes(&input).expect("Should parse valid input");

    assert_eq!(parsed.token_mint, token_mint);
    assert_eq!(parsed.amount, amount);
    assert_eq!(parsed.user, user);
    assert_eq!(parsed.nonce, nonce);
    assert_eq!(parsed.deadline, deadline);
    assert_eq!(parsed.action_tree_root, action_tree_root);
    assert_eq!(parsed.signature, signature);
    assert_eq!(parsed.ed25519_ix_index, ed25519_ix_index);
}

/// Test UnwrapInput parsing round-trip
#[test]
fn test_unwrap_input_parsing_fields() {
    let token_mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let amount: u64 = 500_000;

    let mut input = Vec::with_capacity(UnwrapInput::SIZE);
    input.extend_from_slice(&token_mint.to_bytes());
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&recipient.to_bytes());

    let parsed = UnwrapInput::try_from_bytes(&input).expect("Should parse valid input");

    assert_eq!(parsed.token_mint, token_mint);
    assert_eq!(parsed.amount, amount);
    assert_eq!(parsed.recipient, recipient);
}
