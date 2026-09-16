//! Tests for the wire formats, the nonce bitmap, and the adapter-state read.

use crate::state::{
    base64_of_hash, nonce_to_word_and_bit, pa_is_stopped, NonceBitmap, UnwrapInput, WrapInput,
    WrapMessage, NONCES_PER_WORD, SIGNED_MESSAGE_LEN,
};
use anchor_lang::prelude::Pubkey;
use anchor_lang::AccountSerialize;
use protocol_adapter::state::{PALifecycle, PAStateAccount};

#[test]
fn test_wrap_message_serialization() {
    let msg = WrapMessage {
        forwarder_id: [0xAAu8; 32], // Domain separation - forwarder program ID
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    let bytes = msg.to_bytes();
    assert_eq!(bytes.len(), WrapMessage::SIZE);

    // Verify forwarder_id (domain separation)
    assert_eq!(&bytes[0..32], &[0xAAu8; 32]);

    // Verify token_mint
    assert_eq!(&bytes[32..64], &[1u8; 32]);

    // Verify amount
    assert_eq!(u64::from_le_bytes(bytes[64..72].try_into().unwrap()), 1000);

    // Verify nonce
    assert_eq!(u64::from_le_bytes(bytes[72..80].try_into().unwrap()), 42);

    // Verify deadline
    assert_eq!(
        i64::from_le_bytes(bytes[80..88].try_into().unwrap()),
        1700000000
    );

    // Verify action_tree_root
    assert_eq!(&bytes[88..120], &[2u8; 32]);
}

#[test]
fn test_wrap_message_domain_separation() {
    // Same message with different forwarder IDs should produce different hashes
    let msg1 = WrapMessage {
        forwarder_id: [1u8; 32],
        token_mint: [0u8; 32],
        amount: 100,
        nonce: 1,
        deadline: 1700000000,
        action_tree_root: [0u8; 32],
    };

    let msg2 = WrapMessage {
        forwarder_id: [2u8; 32], // Different forwarder
        token_mint: [0u8; 32],
        amount: 100,
        nonce: 1,
        deadline: 1700000000,
        action_tree_root: [0u8; 32],
    };

    // Hashes must be different due to domain separation
    assert_ne!(msg1.hash(), msg2.hash());
}

#[test]
fn test_wrap_input_parsing() {
    let mut data = vec![0u8; WrapInput::SIZE];

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
fn wrap_input_round_trips_through_to_bytes() {
    let input = WrapInput {
        token_mint: Pubkey::new_unique(),
        amount: 100_000_000,
        user: Pubkey::new_unique(),
        nonce: 7,
        deadline: 4_102_444_800,
        action_tree_root: [0xab; 32],
        signature: [0xcd; 64],
        ed25519_ix_index: 3,
    };
    let bytes = input.to_bytes();
    assert_eq!(bytes.len(), WrapInput::SIZE);
    let parsed = WrapInput::try_from_bytes(&bytes).unwrap();
    assert_eq!(parsed.token_mint, input.token_mint);
    assert_eq!(parsed.amount, input.amount);
    assert_eq!(parsed.user, input.user);
    assert_eq!(parsed.nonce, input.nonce);
    assert_eq!(parsed.deadline, input.deadline);
    assert_eq!(parsed.action_tree_root, input.action_tree_root);
    assert_eq!(parsed.signature, input.signature);
    assert_eq!(parsed.ed25519_ix_index, input.ed25519_ix_index);
}

#[test]
fn unwrap_input_round_trips_through_to_bytes() {
    let input = UnwrapInput {
        token_mint: Pubkey::new_unique(),
        amount: 50_000_000,
        recipient: Pubkey::new_unique(),
    };
    let bytes = input.to_bytes();
    assert_eq!(bytes.len(), UnwrapInput::SIZE);
    let parsed = UnwrapInput::try_from_bytes(&bytes).unwrap();
    assert_eq!(parsed.token_mint, input.token_mint);
    assert_eq!(parsed.amount, input.amount);
    assert_eq!(parsed.recipient, input.recipient);
}

#[test]
fn test_unwrap_input_parsing() {
    let mut data = vec![0u8; UnwrapInput::SIZE];

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
fn wrap_input_rejects_any_other_length() {
    assert!(WrapInput::try_from_bytes(&[0u8; WrapInput::SIZE - 1]).is_err());
    assert!(WrapInput::try_from_bytes(&[0u8; WrapInput::SIZE + 1]).is_err());
    assert!(WrapInput::try_from_bytes(&[0u8; WrapInput::SIZE]).is_ok());
}

#[test]
fn unwrap_input_rejects_any_other_length() {
    assert!(UnwrapInput::try_from_bytes(&[0u8; UnwrapInput::SIZE - 1]).is_err());
    assert!(UnwrapInput::try_from_bytes(&[0u8; UnwrapInput::SIZE + 1]).is_err());
    assert!(UnwrapInput::try_from_bytes(&[0u8; UnwrapInput::SIZE]).is_ok());
}

#[test]
fn base64_of_hash_matches_standard_base64() {
    assert_eq!(
        &base64_of_hash(&[0u8; 32]),
        b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
    );
    // sha256("") in standard base64
    let sha256_empty: [u8; 32] =
        hex_literal::hex!("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    assert_eq!(
        &base64_of_hash(&sha256_empty),
        b"47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="
    );
    let mut all_ones = [0xffu8; 32];
    all_ones[31] = 0xfe;
    assert_eq!(
        &base64_of_hash(&all_ones),
        b"//////////////////////////////////////////4="
    );
}

#[test]
fn signed_message_is_base64_of_the_message_hash() {
    let msg = WrapMessage {
        forwarder_id: [1u8; 32],
        token_mint: [2u8; 32],
        amount: 100,
        nonce: 1,
        deadline: 1700000000,
        action_tree_root: [3u8; 32],
    };
    let signed = msg.signed_message();
    assert_eq!(signed.len(), SIGNED_MESSAGE_LEN);
    assert_eq!(signed, base64_of_hash(&msg.hash()));
    assert!(signed
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(c)));
}

#[test]
fn test_nonce_to_word_and_bit() {
    // Nonce 0 -> word 0, bit 0
    assert_eq!(nonce_to_word_and_bit(0), (0, 0));

    // Nonce 1 -> word 0, bit 1
    assert_eq!(nonce_to_word_and_bit(1), (0, 1));

    // Nonce 255 -> word 0, bit 255
    assert_eq!(nonce_to_word_and_bit(255), (0, 255));

    // Nonce 256 -> word 1, bit 0 (boundary case)
    assert_eq!(nonce_to_word_and_bit(256), (1, 0));

    // Nonce 257 -> word 1, bit 1
    assert_eq!(nonce_to_word_and_bit(257), (1, 1));

    // Large nonce
    assert_eq!(nonce_to_word_and_bit(1000), (3, 232)); // 1000 / 256 = 3, 1000 % 256 = 232

    assert_eq!(NONCES_PER_WORD, 256);
}

#[test]
fn nonce_bitmap_marks_and_reads_every_bit() {
    let mut bitmap = NonceBitmap::default();
    for bit in 0..=255u8 {
        assert!(!bitmap.is_used(bit), "bit {bit} starts unset");
    }

    bitmap.mark_used(0);
    assert!(bitmap.is_used(0));
    assert!(!bitmap.is_used(1));

    bitmap.mark_used(7);
    assert_eq!(bitmap.bits[0], 0b1000_0001);

    bitmap.mark_used(8);
    assert_eq!(bitmap.bits[1], 0b0000_0001);

    bitmap.mark_used(255);
    assert_eq!(bitmap.bits[31], 0b1000_0000);

    bitmap.mark_used(0);
    assert_eq!(bitmap.bits[0], 0b1000_0001, "marking twice is idempotent");
}

// =============================================================================
// PA Emergency Stopped Tests
// =============================================================================

/// A PA state account exactly as the adapter serializes it (discriminator
/// plus the current layout), with the given lifecycle.
fn serialized_pa_state(lifecycle: PALifecycle, pending_authority: Option<Pubkey>) -> Vec<u8> {
    let state = PAStateAccount {
        schema_version: PAStateAccount::SCHEMA_VERSION,
        bump: 254,
        authority: Pubkey::new_unique(),
        verifier_router: Pubkey::new_unique(),
        proof_selector: [0x73, 0xc4, 0x57, 0xba],
        kind_table_commitment: [0u8; 32],
        pending_authority,
        lifecycle,
        root: [7u8; 32],
        next_index: 3,
        current_depth: 2,
        frontier: vec![[1u8; 32], [2u8; 32]],
        min_expiry_slots: 100,
        max_expiry_slots: 216_000,
    };
    let mut data = Vec::new();
    state.try_serialize(&mut data).unwrap();
    data
}

#[test]
fn pa_is_stopped_reads_the_lifecycle_from_the_adapter_layout() {
    for (lifecycle, pending_authority, stopped) in [
        (PALifecycle::Stopped, None, true),
        (PALifecycle::Stopped, Some(Pubkey::new_unique()), true),
        (PALifecycle::Running, Some(Pubkey::new_unique()), false),
    ] {
        let data = serialized_pa_state(lifecycle, pending_authority);
        assert_eq!(pa_is_stopped(&data).unwrap(), stopped, "{lifecycle:?}");
    }
}

#[test]
fn pa_is_stopped_rejects_data_that_is_not_a_pa_state_account() {
    let mut data = serialized_pa_state(PALifecycle::Stopped, None);
    data[0] ^= 0xff; // corrupt the discriminator
    assert!(
        pa_is_stopped(&data).is_err(),
        "a foreign account must be an error, not \"not stopped\""
    );
}

#[test]
fn pa_is_stopped_rejects_truncated_data() {
    let data = serialized_pa_state(PALifecycle::Stopped, None);
    assert!(pa_is_stopped(&data[..40]).is_err());
    assert!(pa_is_stopped(&[]).is_err());
}
