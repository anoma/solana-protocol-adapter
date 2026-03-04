use crate::groth16::{prepare_proof_for_verification, BATCH_AGGREGATION_IMAGE_ID};
use crate::tests::utils::{
    create_minimal_transaction, fake_aggregation_proof_bytes, FAKE_SELECTOR,
};

#[test]
fn test_prepare_proof_accepts_batch_discriminant() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(fake_aggregation_proof_bytes());

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
    tx.aggregation_proof = Some(vec![0xFF, 0xFF, 0xFF]);
    assert!(prepare_proof_for_verification(&tx).is_err());
}

#[test]
fn test_selector_extraction_from_aggregation_proof() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(fake_aggregation_proof_bytes());

    let prepared = prepare_proof_for_verification(&tx).unwrap();
    assert_eq!(
        prepared.seal.selector, FAKE_SELECTOR,
        "Selector should be extracted correctly"
    );
}
