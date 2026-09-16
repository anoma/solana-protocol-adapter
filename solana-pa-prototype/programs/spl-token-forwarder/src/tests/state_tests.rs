//! Tests for state types and PDA derivation

use crate::state::{
    derive_config_pda, derive_escrow_pda, derive_nonce_bitmap_pda, is_nonce_used,
    nonce_to_word_and_bit, pa_is_stopped, set_nonce_used, UnwrapInput, WrapInput, WrapMessage,
    CONFIG_SEED, ESCROW_SEED, NONCES_PER_WORD, NONCE_BITMAP_SEED, NONCE_BITMAP_SIZE,
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

    // Same forwarder_id should produce same hash
    let msg3 = WrapMessage {
        forwarder_id: [1u8; 32],
        token_mint: [0u8; 32],
        amount: 100,
        nonce: 1,
        deadline: 1700000000,
        action_tree_root: [0u8; 32],
    };
    assert_eq!(msg1.hash(), msg3.hash());
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
fn test_pda_derivation() {
    let program_id = Pubkey::new_unique();
    let token_mint = Pubkey::new_unique();
    let user = Pubkey::new_unique();

    // Config PDA
    let (config_pda, config_bump) = derive_config_pda(&program_id);
    let expected_config_pda =
        Pubkey::create_program_address(&[CONFIG_SEED, &[config_bump]], &program_id).unwrap();
    assert_eq!(config_pda, expected_config_pda);
    assert_ne!(config_pda, Pubkey::default());

    // Escrow PDA
    let (escrow_pda, escrow_bump) = derive_escrow_pda(&program_id, &token_mint);
    let expected_escrow_pda = Pubkey::create_program_address(
        &[ESCROW_SEED, token_mint.as_ref(), &[escrow_bump]],
        &program_id,
    )
    .unwrap();
    assert_eq!(escrow_pda, expected_escrow_pda);
    assert_ne!(escrow_pda, Pubkey::default());

    // Nonce Bitmap PDA (Permit2-style)
    // Nonces 0-255 map to word_index 0
    let (bitmap_pda_0, bitmap_bump_0) = derive_nonce_bitmap_pda(&program_id, &user, 0);
    let word_0 = 0u64.to_le_bytes();
    let expected_bitmap_pda_0 = Pubkey::create_program_address(
        &[NONCE_BITMAP_SEED, user.as_ref(), &word_0, &[bitmap_bump_0]],
        &program_id,
    )
    .unwrap();
    assert_eq!(bitmap_pda_0, expected_bitmap_pda_0);
    assert_ne!(bitmap_pda_0, Pubkey::default());

    // Nonces 256-511 map to word_index 1 (different PDA)
    let (bitmap_pda_1, _) = derive_nonce_bitmap_pda(&program_id, &user, 1);
    assert_ne!(bitmap_pda_0, bitmap_pda_1);

    // Same word_index should give same PDA
    let (bitmap_pda_0_again, _) = derive_nonce_bitmap_pda(&program_id, &user, 0);
    assert_eq!(bitmap_pda_0, bitmap_pda_0_again);
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

    // Verify constants
    assert_eq!(NONCES_PER_WORD, 256);
    assert_eq!(NONCE_BITMAP_SIZE, 32);
}

#[test]
fn test_nonce_bitmap_operations() {
    let mut bitmap = [0u8; NONCE_BITMAP_SIZE];

    // Initially all bits should be unset
    for i in 0..=255u8 {
        assert!(!is_nonce_used(&bitmap, i), "Bit {} should be unset", i);
    }

    // Set bit 0
    set_nonce_used(&mut bitmap, 0);
    assert!(is_nonce_used(&bitmap, 0));
    assert!(!is_nonce_used(&bitmap, 1));

    // Set bit 7 (end of first byte)
    set_nonce_used(&mut bitmap, 7);
    assert!(is_nonce_used(&bitmap, 7));
    assert_eq!(bitmap[0], 0b10000001); // bits 0 and 7 set

    // Set bit 8 (start of second byte)
    set_nonce_used(&mut bitmap, 8);
    assert!(is_nonce_used(&bitmap, 8));
    assert_eq!(bitmap[1], 0b00000001);

    // Set bit 255 (last bit of last byte)
    set_nonce_used(&mut bitmap, 255);
    assert!(is_nonce_used(&bitmap, 255));
    assert_eq!(bitmap[31], 0b10000000); // bit 7 of byte 31

    // Verify idempotence - setting same bit twice is safe
    set_nonce_used(&mut bitmap, 0);
    assert!(is_nonce_used(&bitmap, 0));
    assert_eq!(bitmap[0], 0b10000001); // unchanged
}

#[test]
fn test_bitmap_undersized_handling() {
    let small_bitmap = [0u8; 16]; // Less than NONCE_BITMAP_SIZE

    // Should return false for undersized bitmap (not panic)
    assert!(!is_nonce_used(&small_bitmap, 0));
    assert!(!is_nonce_used(&small_bitmap, 255));

    let mut small_mut = [0u8; 16];
    // Should be no-op for undersized bitmap (not panic)
    set_nonce_used(&mut small_mut, 0);
    assert_eq!(small_mut, [0u8; 16]); // unchanged
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
fn pa_is_stopped_reads_stopped_from_the_adapter_layout() {
    let data = serialized_pa_state(PALifecycle::Stopped, None);
    assert!(
        pa_is_stopped(&data).unwrap(),
        "a Stopped adapter state must be reported as stopped"
    );
}

#[test]
fn pa_is_stopped_reads_stopped_with_a_pending_authority() {
    let data = serialized_pa_state(PALifecycle::Stopped, Some(Pubkey::new_unique()));
    assert!(
        pa_is_stopped(&data).unwrap(),
        "a Stopped adapter state with a pending authority must be reported as stopped"
    );
}

#[test]
fn pa_is_stopped_reads_running_from_the_adapter_layout() {
    let data = serialized_pa_state(PALifecycle::Running, Some(Pubkey::new_unique()));
    assert!(
        !pa_is_stopped(&data).unwrap(),
        "a Running adapter state must not be reported as stopped"
    );
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
