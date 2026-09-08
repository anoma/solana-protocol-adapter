use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH};
use crate::state::{PALifecycle, PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use anchor_lang::prelude::*;
use arm_core::merkle_path::PADDING_LEAF;

/// The account exactly as `initialize` writes it for a fresh deployment.
fn fresh_state() -> PAStateAccount {
    PAStateAccount {
        schema_version: PAStateAccount::SCHEMA_VERSION,
        bump: 255,
        authority: Pubkey::new_unique(),
        verifier_router: Pubkey::new_unique(),
        proof_selector: [0; 4],
        kind_table_commitment: [0; 32],
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

/// Same as `fresh_state()`, but with a pending authority set: `Option<Pubkey>`
/// serializes `Some` as 33 bytes, the maximum `BASE_SPACE` reserves for this
/// field, unlike `None`'s 1-byte encoding in `fresh_state()`.
fn state_with_pending_authority() -> PAStateAccount {
    PAStateAccount {
        pending_authority: Some(Pubkey::new_unique()),
        ..fresh_state()
    }
}

fn serialized(state: &PAStateAccount) -> Vec<u8> {
    let mut bytes = Vec::new();
    state
        .try_serialize(&mut bytes)
        .expect("PAStateAccount serializes");
    bytes
}

#[test]
fn schema_version_is_byte_eight_of_the_account_data() {
    let bytes = serialized(&fresh_state());
    assert_eq!(
        bytes[8],
        PAStateAccount::SCHEMA_VERSION,
        "schema version must be the first field after the 8-byte discriminator; a migrate instruction reads it at this offset"
    );
}

#[test]
fn base_space_reserves_the_maximal_pending_authority_encoding() {
    let bytes = serialized(&state_with_pending_authority());
    assert_eq!(
        bytes.len(),
        PAStateAccount::INITIAL_SPACE,
        "serialized state with a pending authority must fill exactly INITIAL_SPACE (BASE_SPACE reserves the 33-byte Some form of pending_authority)"
    );
}

#[test]
fn foreign_schema_version_still_deserializes() {
    // Deserialization alone cannot refuse a foreign layout number: the byte
    // is a plain u8. The per-instruction constraint is what refuses it.
    let mut bytes = serialized(&fresh_state());
    bytes[8] = PAStateAccount::SCHEMA_VERSION + 1;
    let decoded = PAStateAccount::try_deserialize(&mut bytes.as_slice())
        .expect("a foreign version byte is still a well-formed account");
    assert_eq!(decoded.schema_version, PAStateAccount::SCHEMA_VERSION + 1);
}
