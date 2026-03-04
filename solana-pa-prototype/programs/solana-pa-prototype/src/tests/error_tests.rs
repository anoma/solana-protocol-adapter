//! Unit tests for error module.

use crate::error::PAError;
use crate::external_calls::{decode_external_call, verify_output};
use crate::groth16::prepare_proof_for_verification;
use crate::tests::utils::create_minimal_transaction;
use crate::types::ExpirableBlob;
use anchor_lang::solana_program::program_error::ProgramError;

#[test]
fn test_error_external_call_output_mismatch() {
    let expected = vec![1, 2, 3];
    let actual = vec![4, 5, 6];

    let result = verify_output(&expected, &actual);
    assert!(result.is_err());

    match result {
        Err(PAError::ExternalCallOutputMismatch) => {}
        _ => panic!("Expected ExternalCallOutputMismatch error"),
    }
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
        Err(PAError::InvalidProof) => {}
        _ => panic!("Expected InvalidProof error"),
    }
}

#[test]
fn test_program_error_converts_to_cpi_failed() {
    let pa_error = PAError::from(ProgramError::Custom(42));
    match pa_error {
        PAError::ExternalCallCpiFailed => {}
        other => panic!("Expected ExternalCallCpiFailed, got {:?}", other),
    }
}
