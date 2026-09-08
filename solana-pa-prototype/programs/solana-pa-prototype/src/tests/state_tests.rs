use crate::state::PAStateAccount;
use crate::tests::utils::create_test_pa_state;
use anchor_lang::prelude::*;

/// Same as `create_test_pa_state()`, but with a pending authority set:
/// `Option<Pubkey>` serializes `Some` as 33 bytes, the maximum `BASE_SPACE`
/// reserves for this field, unlike `None`'s 1-byte encoding.
fn state_with_pending_authority() -> PAStateAccount {
    PAStateAccount {
        pending_authority: Some(Pubkey::new_unique()),
        ..create_test_pa_state()
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
    let bytes = serialized(&create_test_pa_state());
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
    let mut bytes = serialized(&create_test_pa_state());
    bytes[8] = PAStateAccount::SCHEMA_VERSION + 1;
    let decoded = PAStateAccount::try_deserialize(&mut bytes.as_slice())
        .expect("a foreign version byte is still a well-formed account");
    assert_eq!(decoded.schema_version, PAStateAccount::SCHEMA_VERSION + 1);
}
