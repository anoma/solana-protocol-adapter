//! Curve point validation and arithmetic tests belong in arm_solana.

use crate::merkle::EMPTY_TREE_ROOT_INITIAL;
use crate::tests::utils::build_tx_from_instances;
use arm_core::compliance::ComplianceInstance;
use arm_core::delta_types::{DeltaProof, DeltaWitness};
use arm_core::transaction::Delta;
use arm_core::Digest;
use arm_solana::delta::verify_delta_proof;
use arm_solana::SolanaArmError;

#[test]
fn test_verify_delta_proof_witness_returns_expected_delta_proof() {
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::from_bytes([1u8; 32]),
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
        created_commitment: Digest::from_bytes([2u8; 32]),
        created_logic_ref: Digest::default(),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };
    let mut tx = build_tx_from_instances(&[instance]);
    tx.delta_proof = Delta::Witness(DeltaWitness([0u8; 32]));

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::ExpectedDeltaProof) => {}
        other => panic!("Witness should return ExpectedDeltaProof, got {:?}", other),
    }
}

#[test]
fn test_verify_delta_proof_invalid_point_returns_error() {
    // delta_x/delta_y = [0; 8] represents (0,0) — not on secp256k1.
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::from_bytes([1u8; 32]),
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
        created_commitment: Digest::from_bytes([2u8; 32]),
        created_logic_ref: Digest::default(),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };
    let mut tx = build_tx_from_instances(&[instance]);
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
