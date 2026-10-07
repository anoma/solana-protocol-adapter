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

/// Growing the tree and denying logic refs resize the account to `space()`
/// and serialize into it, so `space()` must be the exact encoding length.
#[test]
fn space_is_the_encoding_of_a_grown_state_with_denials() {
    let mut state = PAStateAccount {
        denied_consumed_logic_refs: vec![[5; 32], [6; 32]],
        denied_created_logic_refs: vec![[7; 32]],
        ..create_test_pa_state()
    };
    state.grow();
    assert_eq!(
        serialized(&state).len(),
        PAStateAccount::space(state.depth(), state.denied_count()),
    );
}

/// pa-evm's two denylists: a consumed resource is checked against the one
/// for consumed resources only, a created one against the one for created
/// resources only.
#[test]
fn each_side_is_checked_against_its_own_denylist() {
    let (consumed_only, created_only) = ([5u8; 32], [6u8; 32]);
    let state = PAStateAccount {
        denied_consumed_logic_refs: vec![consumed_only],
        denied_created_logic_refs: vec![created_only],
        ..create_test_pa_state()
    };
    assert!(state.is_logic_ref_denied(&consumed_only, true));
    assert!(!state.is_logic_ref_denied(&consumed_only, false));
    assert!(state.is_logic_ref_denied(&created_only, false));
    assert!(!state.is_logic_ref_denied(&created_only, true));
}

/// A schema-3 account is this layout with one denylist where this one has
/// two: removing the trailing denylist from an encoding of this layout
/// whose two denylists are equal yields an account the previous build
/// wrote, and migrating it must restore every field, with each previous
/// entry on both denylists.
#[test]
fn migrate_reads_the_previous_layout_field_for_field() {
    use crate::state::{PreviousPAState, PREVIOUS_SCHEMA_VERSION};
    let denied = vec![[5; 32], [6; 32]];
    let mut state = PAStateAccount {
        paused: true,
        next_index: 7,
        min_expiry_slots: 123,
        max_expiry_slots: 4567,
        kind_table_commitment: [9; 32],
        denied_consumed_logic_refs: denied.clone(),
        denied_created_logic_refs: denied.clone(),
        ..create_test_pa_state()
    };
    state.grow();
    state.set_frontier(1, arm_core::Digest::from_bytes([3; 32]));

    let mut previous = serialized(&state);
    previous.truncate(previous.len() - (4 + 32 * denied.len()));
    previous[8] = PREVIOUS_SCHEMA_VERSION;
    let migrated = PreviousPAState::deserialize(&mut &previous[8..])
        .expect("the previous layout deserializes")
        .migrate();

    assert_eq!(
        serialized(&migrated),
        serialized(&state),
        "migrating a schema-3 account must reproduce every field in this layout"
    );
    assert_eq!(
        serialized(&migrated).len(),
        PAStateAccount::space(migrated.depth(), migrated.denied_count()),
        "migrate_state resizes the account to space() and serializes over every byte of it"
    );
}

/// pa-evm's `_isKindTableCommitmentAccepted`: a transaction is proven against
/// the stored kind table or against the empty one, which merges no kinds, so
/// what balances under it also balances under the stored table.
#[test]
fn the_stored_and_the_empty_kind_table_are_accepted_and_no_other() {
    let stored = [7u8; 32];
    let state = PAStateAccount {
        kind_table_commitment: stored,
        ..create_test_pa_state()
    };
    assert!(state.is_kind_table_commitment_accepted(&stored));
    assert!(state.is_kind_table_commitment_accepted(&EMPTY_KIND_TABLE_COMMITMENT));
    assert!(!state.is_kind_table_commitment_accepted(&[8u8; 32]));
}
