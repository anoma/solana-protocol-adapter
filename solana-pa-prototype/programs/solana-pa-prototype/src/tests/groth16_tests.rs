use crate::groth16::prepare_proof_for_verification;
use crate::tests::utils::{fake_aggregation_proof_bytes, minimal_instance, FAKE_SELECTOR};
use arm_core::constants::BATCH_AGGREGATION_VK;
use arm_core::transaction::Aggregation;

fn fake_aggregation() -> Aggregation {
    Aggregation {
        proof: fake_aggregation_proof_bytes(),
        instance: minimal_instance(),
    }
}

#[test]
fn test_prepare_proof_accepts_batch_discriminant() {
    let prepared = prepare_proof_for_verification(&fake_aggregation(), FAKE_SELECTOR).unwrap();
    let expected: [u8; 32] = BATCH_AGGREGATION_VK.into();
    assert_eq!(prepared.image_id, expected);
}

#[test]
fn test_prepare_proof_invalid_bytes() {
    let mut aggregation = fake_aggregation();
    aggregation.proof = vec![0xFF, 0xFF, 0xFF];
    assert!(prepare_proof_for_verification(&aggregation, FAKE_SELECTOR).is_err());
}

#[test]
fn test_selector_extraction_from_aggregation_proof() {
    let prepared = prepare_proof_for_verification(&fake_aggregation(), FAKE_SELECTOR).unwrap();
    assert_eq!(
        prepared.seal.selector, FAKE_SELECTOR,
        "Selector should be extracted correctly"
    );
}

#[test]
fn test_prepare_proof_rejects_wrong_selector() {
    let wrong_selector = [0x00, 0x00, 0x00, 0x00];
    let result = prepare_proof_for_verification(&fake_aggregation(), wrong_selector);
    match result {
        Err(crate::error::PAError::InvalidProofSelector) => {}
        Err(other) => panic!("Expected InvalidProofSelector, got {:?}", other),
        Ok(_) => panic!("Expected error, got Ok"),
    }
}

/// The journal digest must be the sha256 of the instance's journal encoding,
/// so any instance mutation invalidates the prepared proof binding.
#[test]
fn test_journal_digest_binds_instance() {
    let aggregation = fake_aggregation();
    let prepared = prepare_proof_for_verification(&aggregation, FAKE_SELECTOR).unwrap();

    let mut mutated = aggregation.clone();
    mutated.instance.actions[0].action_tree_root = arm_core::Digest::from_bytes([0xEE; 32]);
    let prepared_mutated = prepare_proof_for_verification(&mutated, FAKE_SELECTOR).unwrap();

    assert_ne!(
        prepared.journal_digest, prepared_mutated.journal_digest,
        "instance mutation must change the journal digest"
    );
}
