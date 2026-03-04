//! Unit tests for the PA's delta proof verification.
//!
//! These tests exercise `crate::delta::verify_delta_proof`, which delegates to
//! `arm_solana::delta::verify_delta_proof` with error conversion via `From<SolanaArmError>`.
//! Curve point validation and arithmetic tests belong in arm_solana.

use crate::delta::verify_delta_proof;
use crate::error::PAError;
use crate::tests::utils::{build_tx_from_instances, create_compliance_instance};
use crate::types::{Delta, DeltaWitness, Digest};
use arm_core::delta_types::DeltaProof;

#[test]
fn test_verify_delta_proof_witness_returns_expected_delta_proof() {
    let instance =
        create_compliance_instance(Digest::from_bytes([1u8; 32]), Digest::from_bytes([2u8; 32]));
    let mut tx = build_tx_from_instances(&[instance]);
    tx.delta_proof = Delta::Witness(DeltaWitness([0u8; 32]));

    match verify_delta_proof(&tx) {
        Err(PAError::ExpectedDeltaProof) => {}
        other => panic!(
            "Witness should map to PAError::ExpectedDeltaProof, got {:?}",
            other
        ),
    }
}

#[test]
fn test_verify_delta_proof_invalid_point_returns_pa_error() {
    let instance =
        create_compliance_instance(Digest::from_bytes([1u8; 32]), Digest::from_bytes([2u8; 32]));
    // create_compliance_instance sets delta_x/delta_y to [0; 8], which is (0,0) — not on curve.
    let mut tx = build_tx_from_instances(&[instance]);
    // Use Delta::Proof so verify_delta_proof reaches accumulate_deltas (which rejects the point).
    tx.delta_proof = Delta::Proof(DeltaProof([0u8; 65]));

    match verify_delta_proof(&tx) {
        Err(PAError::DeltaPointNotOnCurve) => {}
        other => panic!(
            "Invalid delta point should map to PAError::DeltaPointNotOnCurve, got {:?}",
            other
        ),
    }
}
