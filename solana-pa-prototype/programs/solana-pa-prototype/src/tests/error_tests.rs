//! Unit tests for error module.

use crate::error::PAError;
use crate::external_calls::{decode_external_call, verify_output};
use crate::groth16::prepare_proof_for_verification;
use crate::tests::utils::create_minimal_transaction;
use crate::txdata::TxData;
use crate::types::{ExpirableBlob, OutputMode};

#[test]
fn test_error_external_call_output_mismatch() {
    let expected = vec![1, 2, 3];
    let actual = vec![4, 5, 6];

    let result = verify_output(&expected, &actual, &OutputMode::ReturnData);
    assert!(result.is_err());

    match result {
        Err(PAError::ExternalCallOutputMismatch) => {}
        _ => panic!("Expected ExternalCallOutputMismatch error"),
    }
}

#[test]
fn test_error_txdata_expired() {
    let txdata = TxData::new(100, 1000);
    let current_slot = 2000;

    let result = txdata.validate_not_expired(current_slot);
    assert!(result.is_err());
}

#[test]
fn test_error_invalid_external_call_blob() {
    let blob = ExpirableBlob {
        blob: vec![0xDEAD, 0xBEEF], // Garbage
        deletion_criterion: 0,
    };

    let result = decode_external_call(&blob);
    match result {
        Err(PAError::InvalidExternalCallBlob) => {}
        _ => panic!("Expected InvalidExternalCallBlob error"),
    }
}

#[test]
fn test_error_invalid_aggregation_proof_bytes() {
    let mut tx = create_minimal_transaction();
    tx.aggregation_proof = Some(vec![0xFF; 10]); // Invalid

    let result = prepare_proof_for_verification(&tx);
    match result {
        Err(PAError::InvalidProof) | Err(PAError::UnsupportedProofType) => {}
        _ => panic!("Expected InvalidProof or UnsupportedProofType error"),
    }
}
