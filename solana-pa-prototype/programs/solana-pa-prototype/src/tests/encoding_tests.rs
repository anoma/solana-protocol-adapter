//! Unit tests for encoding module.

use crate::encoding::{
    compute_action_tree_root, compute_batch_aggregation_journal_digest, find_logic_input,
};
use crate::error::PAError;
use crate::merkle;
use crate::tests::utils::create_minimal_transaction;
use crate::types::*;

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
    let consumed_tag = cu.instance.consumed_nullifier;

    let found = find_logic_input(&action.logic_verifier_inputs, &consumed_tag);
    assert!(found.is_ok());
    assert_eq!(found.unwrap().tag, consumed_tag);
}

#[test]
fn test_find_logic_input_not_found() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let missing_tag = Digest::from_bytes([0xFF; 32]);
    match find_logic_input(&action.logic_verifier_inputs, &missing_tag) {
        Err(PAError::TagNotFound) => {}
        other => panic!("Expected TagNotFound, got {:?}", other),
    }
}

#[test]
fn test_tag_count_invariant() {
    let tx = create_minimal_transaction();
    let action = &tx.actions[0];

    let expected = action.compliance_units.len() * 2;
    let actual = action.logic_verifier_inputs.len();
    assert_eq!(actual, expected);
}

// =========================================================================
// BATCH JOURNAL DIGEST TESTS
// =========================================================================

#[test]
fn test_batch_journal_digest_tag_count_mismatch() {
    // 1 CU produces 2 tags, but we provide only 1 LVI — should be rejected.
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::from_bytes([1u8; 32]),
        consumed_logic_ref: Digest::from_bytes([3u8; 32]),
        consumed_commitment_tree_root: Digest::default(),
        created_commitment: Digest::from_bytes([2u8; 32]),
        created_logic_ref: Digest::from_bytes([4u8; 32]),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };
    let tx = Transaction {
        actions: vec![Action {
            compliance_units: vec![ComplianceUnit {
                proof: None,
                instance,
            }],
            logic_verifier_inputs: vec![LogicVerifierInputs {
                tag: Digest::from_bytes([1u8; 32]),
                verifying_key: Digest::from_bytes([3u8; 32]),
                app_data: AppData::default(),
                proof: None,
                instance_journal: Vec::new(),
            }],
        }],
        delta_proof: Delta::Witness(DeltaWitness([0u8; 32])),
        expected_balance: None,
        aggregation_proof: None,
    };

    match compute_batch_aggregation_journal_digest(&tx) {
        Err(PAError::InvalidTransactionData) => {}
        other => panic!(
            "Expected InvalidTransactionData for tag/LVI count mismatch, got {:?}",
            other
        ),
    }
}

#[test]
fn test_batch_journal_digest_vk_mismatch() {
    let mut tx = create_minimal_transaction();
    // The consumed LVI's verifying_key should match consumed_logic_ref = [3u8; 32].
    // Set it to something wrong to trigger the VK mismatch check.
    tx.actions[0].logic_verifier_inputs[0].verifying_key = Digest::from_bytes([0xFF; 32]);

    match compute_batch_aggregation_journal_digest(&tx) {
        Err(PAError::InvalidTransactionData) => {}
        other => panic!(
            "Expected InvalidTransactionData for VK mismatch, got {:?}",
            other
        ),
    }
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

    // Mutate a field and verify the digest changes.
    let mut tx_mutated = create_minimal_transaction();
    tx_mutated.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier = Digest::from_bytes([0xFF; 32]);
    // Update the corresponding LVI tag so find_logic_input still succeeds.
    tx_mutated.actions[0].logic_verifier_inputs[0].tag = Digest::from_bytes([0xFF; 32]);

    let digest3 = compute_batch_aggregation_journal_digest(&tx_mutated).unwrap();
    assert_ne!(
        digest1, digest3,
        "Different transaction data must produce different digest"
    );
}
