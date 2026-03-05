//! Curve point validation and arithmetic tests belong in arm_solana.

use crate::tests::utils::create_minimal_transaction;
use arm_core::delta_types::{DeltaProof, DeltaWitness};
use arm_core::transaction::Delta;
use arm_solana::delta::verify_delta_proof;
use arm_solana::SolanaArmError;

#[test]
fn test_verify_delta_proof_witness_returns_expected_delta_proof() {
    // create_minimal_transaction has delta_x/delta_y = [0;8] (point not on secp256k1).
    let mut tx = create_minimal_transaction();
    tx.delta_proof = Delta::Witness(DeltaWitness([0u8; 32]));

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::ExpectedDeltaProof) => {}
        other => panic!("Witness should return ExpectedDeltaProof, got {:?}", other),
    }
}

#[test]
fn test_verify_delta_proof_invalid_point_returns_error() {
    let mut tx = create_minimal_transaction();
    // Use Delta::Proof so verify_delta_proof reaches accumulate_deltas (which rejects the point).
    tx.delta_proof = Delta::Proof(DeltaProof([0u8; 65]));

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::DeltaPointNotOnCurve) => {}
        other => panic!(
            "Invalid delta point should return DeltaPointNotOnCurve, got {:?}",
            other
        ),
    }
}
