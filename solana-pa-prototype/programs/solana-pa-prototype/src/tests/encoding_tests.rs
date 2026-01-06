//! Unit tests for encoding module.

use crate::encoding::{compute_action_tree_root, find_logic_input};
use crate::merkle;
use crate::tests::utils::create_minimal_transaction;
use crate::types::{ComplianceInstance, Digest, OutputMode, SolanaExternalCall};

#[cfg(feature = "non-aggregated-proofs")]
use crate::encoding::{compute_action_tree_root_from_action, extract_tags_and_logic_refs};

// =========================================================================
// OUTPUT MODE ENCODING TESTS
// =========================================================================

#[test]
fn test_output_account_encoding() {
    let call = SolanaExternalCall {
        program_id: [0; 32],
        instruction_data: vec![],
        expected_output: vec![0u8; 2048],
        output_mode: OutputMode::OutputAccount {
            index: 5,
            offset: 100,
            len: 2048,
        },
    };

    let bytes = bincode::serialize(&call).unwrap();
    let decoded: SolanaExternalCall = bincode::deserialize(&bytes).unwrap();

    match decoded.output_mode {
        OutputMode::OutputAccount { index, offset, len } => {
            assert_eq!(index, 5);
            assert_eq!(offset, 100);
            assert_eq!(len, 2048);
        }
        _ => panic!("Expected OutputAccount"),
    }
}

// =========================================================================
// ACTION TREE ROOT TESTS
// =========================================================================

#[test]
fn test_action_tree_root_single_cu() {
    let tag1 = Digest::from_bytes([0x11; 32]);
    let tag2 = Digest::from_bytes([0x22; 32]);
    let tags = vec![tag1, tag2];

    let root = compute_action_tree_root(&tags).expect("should compute root");
    let expected = merkle::hash_two(&tag1, &tag2);
    assert_eq!(root, expected);
}

#[test]
fn test_action_tree_root_two_cus() {
    let tag1 = Digest::from_bytes([0x11; 32]);
    let tag2 = Digest::from_bytes([0x22; 32]);
    let tag3 = Digest::from_bytes([0x33; 32]);
    let tag4 = Digest::from_bytes([0x44; 32]);
    let tags = vec![tag1, tag2, tag3, tag4];

    let root = compute_action_tree_root(&tags).expect("should compute root");
    let left = merkle::hash_two(&tag1, &tag2);
    let right = merkle::hash_two(&tag3, &tag4);
    let expected = merkle::hash_two(&left, &right);
    assert_eq!(root, expected);
}

#[test]
fn test_action_tree_root_three_tags_padded() {
    let tag1 = Digest::from_bytes([0x11; 32]);
    let tag2 = Digest::from_bytes([0x22; 32]);
    let tag3 = Digest::from_bytes([0x33; 32]);
    let tags = vec![tag1, tag2, tag3];

    let root = compute_action_tree_root(&tags).expect("should compute root");
    let left = merkle::hash_two(&tag1, &tag2);
    let right = merkle::hash_two(&tag3, &merkle::PADDING_LEAF);
    let expected = merkle::hash_two(&left, &right);
    assert_eq!(root, expected);
}

// =========================================================================
// TAG EXTRACTION AND LOGIC INPUT TESTS
// =========================================================================

#[test]
fn test_find_logic_input_by_tag() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let cu = &action.compliance_units[0];
    let instance: ComplianceInstance = bincode::deserialize(&cu.instance).unwrap();
    let consumed_tag = instance.consumed_nullifier;

    let found = find_logic_input(&action.logic_verifier_inputs, &consumed_tag);
    assert!(found.is_ok());
    assert_eq!(found.unwrap().tag, consumed_tag);
}

#[test]
fn test_find_logic_input_not_found() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let missing_tag = Digest::from_bytes([0xFF; 32]);
    assert!(find_logic_input(&action.logic_verifier_inputs, &missing_tag).is_err());
}

#[test]
fn test_tag_count_invariant() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let expected = action.compliance_units.len() * 2;
    let actual = action.logic_verifier_inputs.len();
    assert_eq!(actual, expected);
}

#[cfg(feature = "non-aggregated-proofs")]
#[test]
fn test_extract_tags_and_logic_refs() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let (tags, logic_refs) = extract_tags_and_logic_refs(action).expect("should extract");
    assert_eq!(tags.len(), 2);
    assert_eq!(logic_refs.len(), 2);

    let cu = &action.compliance_units[0];
    let instance: ComplianceInstance = bincode::deserialize(&cu.instance).unwrap();
    assert_eq!(tags[0], instance.consumed_nullifier);
    assert_eq!(tags[1], instance.created_commitment);
    assert_eq!(logic_refs[0], instance.consumed_logic_ref);
    assert_eq!(logic_refs[1], instance.created_logic_ref);
}

#[cfg(feature = "non-aggregated-proofs")]
#[test]
fn test_action_tree_root_from_action_matches_manual() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let root_from_action = compute_action_tree_root_from_action(action).expect("from action");
    let (tags, _) = extract_tags_and_logic_refs(action).expect("extract");
    let root_manual = compute_action_tree_root(&tags).expect("manual");

    assert_eq!(root_from_action, root_manual);
}
