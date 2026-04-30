use crate::encoding::{
    compute_action_tree_root, compute_batch_aggregation_journal_digest, find_logic_input,
};
use crate::error::PAError;
use crate::merkle;
use crate::tests::utils::create_minimal_transaction;
use arm_core::Digest;

#[test]
fn test_action_tree_root_empty_fails() {
    let result = compute_action_tree_root(&[]);
    assert!(result.is_err(), "empty tags should return error");
}

#[test]
fn test_action_tree_root_single_tag() {
    let tag = Digest::from_bytes([0x11; 32]);
    let root = compute_action_tree_root(&[tag]).expect("should compute root");
    // Single tag: next_power_of_two(1) = 1, no hashing needed — root is the tag itself.
    assert_eq!(root, tag);
}

#[test]
fn test_action_tree_root_two_tags() {
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

#[test]
fn test_find_logic_input_by_tag() {
    use crate::tests::utils::decode_cu_instance;

    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let cu = &action.compliance_units[0];
    let consumed_tag = decode_cu_instance(cu).consumed_nullifier;

    let found = find_logic_input(&action.logic_verifier_inputs, &consumed_tag)
        .expect("should find consumed tag");
    assert_eq!(found.tag, consumed_tag);
}

#[test]
fn test_find_logic_input_not_found() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let missing_tag = Digest::from_bytes([0xFF; 32]);
    assert!(matches!(
        find_logic_input(&action.logic_verifier_inputs, &missing_tag),
        Err(PAError::TagNotFound)
    ));
}

#[test]
fn test_tag_count_invariant() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let expected = action.compliance_units.len() * 2;
    let actual = action.logic_verifier_inputs.len();
    assert_eq!(actual, expected);
}

#[test]
fn test_batch_journal_digest_tag_count_mismatch() {
    // 1 CU produces 2 tags, but we provide only 1 LVI — should be rejected.
    let mut tx = create_minimal_transaction();
    tx.actions[0].logic_verifier_inputs.pop();

    assert!(matches!(
        compute_batch_aggregation_journal_digest(&tx),
        Err(PAError::InvalidTransactionData)
    ));
}

#[test]
fn test_batch_journal_digest_vk_mismatch() {
    let mut tx = create_minimal_transaction();
    // The consumed LVI's verifying_key should match consumed_logic_ref = [3u8; 32].
    // Set it to something wrong to trigger the VK mismatch check.
    tx.actions[0].logic_verifier_inputs[0].verifying_key = Digest::from_bytes([0xFF; 32]);

    assert!(matches!(
        compute_batch_aggregation_journal_digest(&tx),
        Err(PAError::InvalidTransactionData)
    ));
}

#[test]
fn test_batch_journal_digest_deterministic_and_sensitive() {
    let tx = create_minimal_transaction();
    let digest1 = compute_batch_aggregation_journal_digest(&tx).unwrap();
    let digest2 = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_eq!(
        digest1, digest2,
        "Same transaction must produce identical digest"
    );

    let mut tx_mutated = create_minimal_transaction();
    crate::tests::utils::mutate_cu_instance(
        &mut tx_mutated.actions[0].compliance_units[0],
        |inst| inst.consumed_nullifier = Digest::from_bytes([0xFF; 32]),
    );
    // Update the corresponding LVI tag so find_logic_input still succeeds.
    tx_mutated.actions[0].logic_verifier_inputs[0].tag = Digest::from_bytes([0xFF; 32]);

    let digest3 = compute_batch_aggregation_journal_digest(&tx_mutated).unwrap();
    assert_ne!(
        digest1, digest3,
        "Different transaction data must produce different digest"
    );
}
