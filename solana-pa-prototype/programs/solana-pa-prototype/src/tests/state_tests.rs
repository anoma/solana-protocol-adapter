use crate::state::{PAStateAccount, EMPTY_KIND_TABLE_COMMITMENT, SCHEMA_VERSION};
use crate::tests::utils::create_test_pa_state;
use anchor_lang::prelude::*;
use arm_core::compliance::hash_kind_table_entries;

fn serialized(state: &PAStateAccount) -> Vec<u8> {
    let mut bytes = Vec::new();
    state
        .try_serialize(&mut bytes)
        .expect("PAStateAccount serializes");
    bytes
}

#[test]
fn schema_version_is_byte_eight_of_the_account_data() {
    // 0xA7 cannot be mistaken for any other single-byte field's value.
    let state = PAStateAccount {
        schema_version: 0xA7,
        ..create_test_pa_state()
    };
    assert_eq!(
        serialized(&state)[8],
        0xA7,
        "schema version must be the first field after the 8-byte discriminator; a migrate instruction reads it at this offset"
    );
}

#[test]
fn initial_space_is_the_initial_states_encoding() {
    assert_eq!(
        serialized(&create_test_pa_state()).len(),
        PAStateAccount::INITIAL_SPACE,
        "the state initialize writes must fill exactly INITIAL_SPACE"
    );
}

/// Anchor ignores trailing bytes, so an older binary can read a newer
/// account that only appended fields; the per-instruction version
/// constraint is what refuses that.
#[test]
fn trailing_bytes_still_deserialize() {
    let mut bytes = serialized(&create_test_pa_state());
    bytes.extend_from_slice(&[0xFF; 16]);
    let decoded = PAStateAccount::try_deserialize(&mut bytes.as_slice())
        .expect("trailing bytes appended by a newer layout must not break deserialization");
    assert_eq!(decoded.schema_version, SCHEMA_VERSION);
}

#[test]
fn empty_kind_table_commitment_is_the_commitment_of_no_entries() {
    assert_eq!(
        EMPTY_KIND_TABLE_COMMITMENT,
        <[u8; 32]>::from(hash_kind_table_entries(&[])),
        "the constant initialize stores must be the ARM's commitment of the empty kind table"
    );
}

/// Growing the tree and denying a logic ref resize the account to `space()`
/// and serialize into it, so `space()` must be the exact encoding length.
#[test]
fn space_is_the_encoding_of_a_grown_state_with_denials() {
    let mut state = PAStateAccount {
        denied_logic_refs: vec![[5; 32], [6; 32]],
        ..create_test_pa_state()
    };
    state.grow();
    assert_eq!(
        serialized(&state).len(),
        PAStateAccount::space(state.depth(), state.denied_logic_refs.len()),
    );
}
