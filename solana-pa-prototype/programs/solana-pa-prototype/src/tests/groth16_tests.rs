//! Unit tests for groth16 module.

use crate::groth16::{prepare_proof_for_verification, BATCH_AGGREGATION_IMAGE_ID};
use crate::tests::utils::{
    create_minimal_transaction, fake_aggregation_proof_bytes, FAKE_SELECTOR,
};

#[test]
fn test_image_id_constants_match_arm_risc0() {
    // Batch: 5eeeb4e5b4db4548d6c0e21c35b54041cdceda63700b060470826ee2c92740a1
    assert_eq!(
        hex::encode(BATCH_AGGREGATION_IMAGE_ID),
        "5eeeb4e5b4db4548d6c0e21c35b54041cdceda63700b060470826ee2c92740a1"
    );
}

#[test]
fn test_prepare_proof_accepts_batch_discriminant() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(fake_aggregation_proof_bytes(1, 256));

    let prepared = prepare_proof_for_verification(&tx).unwrap();
    assert_eq!(prepared.image_id, BATCH_AGGREGATION_IMAGE_ID);
}

#[test]
fn test_prepare_proof_ignores_strategy_discriminant_like_evm_pa() {
    // If a sequential aggregation proof is submitted, the PA does not explicitly reject it.
    // It attempts verification as "batch" and the verifier should reject it later.
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(fake_aggregation_proof_bytes(0, 256));

    let prepared = prepare_proof_for_verification(&tx).unwrap();
    assert_eq!(prepared.image_id, BATCH_AGGREGATION_IMAGE_ID);
}

#[test]
fn test_prepare_proof_no_proof() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = None;
    assert!(prepare_proof_for_verification(&tx).is_err());
}

#[test]
fn test_prepare_proof_invalid_bytes() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(vec![0xFF, 0xFF, 0xFF]); // Invalid
    assert!(prepare_proof_for_verification(&tx).is_err());
}

#[test]
fn test_selector_extraction_from_aggregation_proof() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(fake_aggregation_proof_bytes(1, 256));

    let prepared = prepare_proof_for_verification(&tx).unwrap();
    assert_eq!(
        prepared.selector, FAKE_SELECTOR,
        "Selector should be extracted from verifier_parameters at tail of proof"
    );
}
