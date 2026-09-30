//! Tests for the wire formats, the nonce bitmap, and the adapter-state read.

use crate::state::{
    base64_of_hash, nonce_to_word_and_bit, pa_is_stopped, NonceBitmap, UnwrapInput, WrapInput,
    WrapMessage, CONFIG_PDA, CONFIG_SEED, ESCROW_AUTHORITY, ESCROW_AUTHORITY_BUMP, ESCROW_SEED,
    NONCES_PER_WORD, NONCE_BITMAP_SEED, PREVIOUS_CONFIG_SIZE, PREVIOUS_NONCE_BITMAP_SIZE,
    SIGNED_MESSAGE_LEN,
};
use anchor_lang::prelude::{borsh, Pubkey};
use anchor_lang::AccountSerialize;
use protocol_adapter::state::{PALifecycle, PAStateAccount};

/// The compile-time config address is the canonical PDA `initialize`
/// creates, the only address a config can live at.
#[test]
fn config_pda_is_the_canonical_config_address() {
    let (canonical, _) = Pubkey::find_program_address(&[CONFIG_SEED], &crate::ID);
    assert_eq!(
        CONFIG_PDA, canonical,
        "CONFIG_PDA must equal find_program_address([CONFIG_SEED], program id)"
    );
}

/// The compile-time escrow authority is the canonical PDA of the escrow
/// seed: the one address that owns every mint's escrow token account, and
/// the bump that signs for it.
#[test]
fn escrow_authority_is_the_canonical_escrow_address_and_bump() {
    let canonical = Pubkey::find_program_address(&[ESCROW_SEED], &crate::ID);
    assert_eq!(
        (ESCROW_AUTHORITY, ESCROW_AUTHORITY_BUMP),
        canonical,
        "(ESCROW_AUTHORITY, ESCROW_AUTHORITY_BUMP) must equal find_program_address([ESCROW_SEED], program id)"
    );
}

/// The field-by-field hash is sha256 of the message's 120-byte Borsh encoding.
#[test]
fn wrap_message_hash_is_sha256_of_its_borsh_encoding() {
    let msg = WrapMessage {
        forwarder_id: [0xAAu8; 32],
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: -1_700_000_000,
        action_tree_root: [2u8; 32],
    };
    let encoded = borsh::to_vec(&msg).unwrap();
    assert_eq!(encoded.len(), WrapMessage::SIZE);
    assert_eq!(msg.hash(), solana_sha256_hasher::hash(&encoded).to_bytes());
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

/// The client library encodes the wrap input this program parses, and the
/// message it recomputes from that input is the one the client tells the
/// wallet to sign.
#[test]
fn wrap_input_parses_the_client_encoding_and_recomputes_its_signed_message() {
    let (mint, user, root) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    let encoded = anoma_pa_solana_client::encode_wrap_forwarder_input(
        &mint,
        1000,
        &user,
        42,
        1_700_000_000,
        &root,
        3,
    );
    let (op, operand) = encoded.split_first().unwrap();
    assert_eq!(*op, crate::OP_WRAP);
    let input = WrapInput::try_from_bytes(operand).unwrap();
    assert_eq!(input.token_mint.to_bytes(), mint);
    assert_eq!(input.amount, 1000);
    assert_eq!(input.user.to_bytes(), user);
    assert_eq!(input.nonce, 42);
    assert_eq!(input.deadline, 1_700_000_000);
    assert_eq!(input.action_tree_root, root);
    assert_eq!(input.ed25519_ix_index, 3);

    let client_message = anoma_pa_solana_client::WrapMessage {
        forwarder_id: crate::ID.to_bytes(),
        token_mint: mint,
        amount: 1000,
        nonce: 42,
        deadline: 1_700_000_000,
        action_tree_root: root,
    };
    assert_eq!(
        input.to_message(&crate::ID).signed_message(),
        client_message.base64_digest().as_bytes()
    );
}

/// The return byte the resource's external call expects is the one this
/// program returns.
#[test]
fn result_success_is_the_client_constant() {
    assert_eq!(
        crate::RESULT_SUCCESS,
        anoma_pa_solana_client::FORWARDER_RESULT_SUCCESS
    );
}

/// The client library encodes the unwrap input this program parses.
#[test]
fn unwrap_input_parses_the_client_encoding() {
    let (mint, recipient) = ([4u8; 32], [5u8; 32]);
    let encoded = anoma_pa_solana_client::encode_unwrap_forwarder_input(&mint, 77, &recipient);
    let (op, operand) = encoded.split_first().unwrap();
    assert_eq!(*op, crate::OP_UNWRAP);
    let input = UnwrapInput::try_from_bytes(operand).unwrap();
    assert_eq!(input.token_mint.to_bytes(), mint);
    assert_eq!(input.amount, 77);
    assert_eq!(input.recipient.to_bytes(), recipient);
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

/// The canonical nonce bitmap address of `user`'s word `word_index`, and its bump.
fn canonical_bitmap(user: &Pubkey, word_index: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[NONCE_BITMAP_SEED, user.as_ref(), &word_index.to_le_bytes()],
        &crate::ID,
    )
}

/// A bitmap is recognized at the address its seeds and stored bump derive,
/// and nowhere else: not as another user's bitmap, not as another word's.
#[test]
fn nonce_bitmap_is_at_only_the_address_of_its_own_user_and_word() {
    let user = Pubkey::new_from_array([7u8; 32]);
    let other_user = Pubkey::new_from_array([8u8; 32]);
    let (address, bump) = canonical_bitmap(&user, 3);
    let bitmap = NonceBitmap {
        bump,
        ..NonceBitmap::default()
    };
    assert!(
        bitmap.is_at(&address, &crate::ID, &user, 3),
        "the bitmap created for (user, word 3) must be accepted for (user, word 3)"
    );
    assert!(
        !bitmap.is_at(&address, &crate::ID, &other_user, 3),
        "the bitmap of (user, word 3) must be rejected for another user"
    );
    assert!(
        !bitmap.is_at(&address, &crate::ID, &user, 4),
        "the bitmap of (user, word 3) must be rejected for another word"
    );

    let (other_address, other_bump) = canonical_bitmap(&other_user, 3);
    let other_bitmap = NonceBitmap {
        bump: other_bump,
        ..NonceBitmap::default()
    };
    assert!(
        !other_bitmap.is_at(&other_address, &crate::ID, &user, 3),
        "another user's bitmap passed for (user, word 3) must be rejected"
    );
}

/// The check binds the address to the stored bump: the canonical address is
/// not accepted under any other bump, so only the bump `init_nonce_bitmap`
/// stores (Anchor's canonical `ctx.bumps`) recognizes it.
#[test]
fn nonce_bitmap_address_check_uses_the_stored_bump() {
    let user = Pubkey::new_from_array([9u8; 32]);
    let (address, canonical_bump) = canonical_bitmap(&user, 0);
    for bump in (0..=u8::MAX).filter(|b| *b != canonical_bump) {
        let bitmap = NonceBitmap {
            bump,
            ..NonceBitmap::default()
        };
        assert!(
            !bitmap.is_at(&address, &crate::ID, &user, 0),
            "bump {bump} is not the canonical {canonical_bump} and must not reach the canonical address"
        );
    }
}

#[test]
fn nonce_bitmap_account_is_discriminator_word_and_bump() {
    assert_eq!(NonceBitmap::ACCOUNT_SIZE, 8 + 32 + 1);
}

// =============================================================================
// PA Emergency Stopped Tests
// =============================================================================

/// A PA state account exactly as the adapter serializes it (discriminator
/// plus the current layout), with the given lifecycle and denied logic refs.
fn serialized_pa_state(lifecycle: PALifecycle, denied_logic_refs: Vec<[u8; 32]>) -> Vec<u8> {
    let state = PAStateAccount {
        lifecycle,
        denied_logic_refs,
        ..PAStateAccount::running(
            254,
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            [0x73, 0xc4, 0x57, 0xba],
        )
    };
    let mut data = Vec::new();
    state.try_serialize(&mut data).unwrap();
    data
}

#[test]
fn pa_is_stopped_reads_the_lifecycle_from_the_adapter_layout() {
    for (lifecycle, denied_logic_refs, stopped) in [
        (PALifecycle::Stopped, vec![], true),
        (PALifecycle::Stopped, vec![[7u8; 32]], true),
        (PALifecycle::Running, vec![[7u8; 32]], false),
    ] {
        let data = serialized_pa_state(lifecycle, denied_logic_refs);
        assert_eq!(pa_is_stopped(&data).unwrap(), stopped, "{lifecycle:?}");
    }
}

#[test]
fn pa_is_stopped_rejects_data_that_is_not_a_pa_state_account() {
    let mut data = serialized_pa_state(PALifecycle::Stopped, vec![]);
    data[0] ^= 0xff; // corrupt the discriminator
    assert!(
        pa_is_stopped(&data).is_err(),
        "a foreign account must be an error, not \"not stopped\""
    );
}

#[test]
fn pa_is_stopped_rejects_truncated_data() {
    let data = serialized_pa_state(PALifecycle::Stopped, vec![]);
    assert!(pa_is_stopped(&data[..40]).is_err());
    assert!(pa_is_stopped(&[]).is_err());
}

/// The previous build's account sizes the migrations require, as its devnet
/// deployment holds them (config `8woJSG…`, 137 bytes; nonce bitmap
/// `Ge6BSx…`, 40 bytes).
#[test]
fn previous_layout_sizes_are_the_deployed_builds() {
    assert_eq!(PREVIOUS_CONFIG_SIZE, 137);
    assert_eq!(PREVIOUS_NONCE_BITMAP_SIZE, 40);
}
