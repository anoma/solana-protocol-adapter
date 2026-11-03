use crate::state::{PAStateAccount, EMPTY_KIND_TREE_ROOT, SCHEMA_VERSION};
use crate::tests::utils::create_test_pa_state;
use anchor_lang::prelude::*;
use arm_core::merkle_path::PADDING_LEAF;

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
fn empty_kind_tree_root_is_the_padding_leaf() {
    assert_eq!(
        EMPTY_KIND_TREE_ROOT,
        <[u8; 32]>::from(PADDING_LEAF),
        "the constant initialize stores must be the ARM's root of the empty kind tree, its padding leaf"
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

/// pa-evm's `_isKindTreeRootAccepted`: a transaction is proven against
/// the stored kind tree or against the empty one, which merges no kinds, so
/// what balances under it also balances under the stored tree.
#[test]
fn the_stored_and_the_empty_kind_tree_are_accepted_and_no_other() {
    let stored = [7u8; 32];
    let state = PAStateAccount {
        kind_tree_root: stored,
        ..create_test_pa_state()
    };
    assert!(state.is_kind_tree_root_accepted(&stored));
    assert!(state.is_kind_tree_root_accepted(&EMPTY_KIND_TREE_ROOT));
    assert!(!state.is_kind_tree_root_accepted(&[8u8; 32]));
}
