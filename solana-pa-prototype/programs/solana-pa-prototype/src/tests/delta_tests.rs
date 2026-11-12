//! Curve point validation and arithmetic tests belong in arm_solana.

use crate::tests::utils::create_minimal_transaction;
use arm_core::delta_proof::{DeltaProof, DeltaWitness};
use arm_core::transaction::Delta;
use arm_solana::delta::{collect_roots, compute_delta_msg_hash, verify_delta_proof};
use arm_solana::journal::require_aggregation;
use arm_solana::SolanaArmError;
use k256::ecdsa::SigningKey;

#[test]
fn test_verify_delta_proof_witness_returns_expected_delta_proof() {
    let mut tx = create_minimal_transaction();
    tx.delta_proof =
        Delta::Witness(DeltaWitness::from_bytes(&[1u8; 32]).expect("scalar 0x0101… is in range"));

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::ExpectedDeltaProof) => {}
        other => panic!("Witness should return ExpectedDeltaProof, got {:?}", other),
    }
}

#[test]
fn a_delta_proof_signed_by_another_key_is_a_mismatch() {
    // A valid signature over the transaction's delta message, by a key whose
    // public key is not the transaction's delta.
    let mut tx = create_minimal_transaction();
    let instance = require_aggregation(&tx).unwrap();
    let msg_hash = compute_delta_msg_hash(&collect_roots(instance));
    let key = SigningKey::from_bytes(&[2u8; 32].into()).unwrap();
    let (signature, recovery_id) = key.sign_prehash_recoverable(&msg_hash).unwrap();
    let mut bytes = signature.to_bytes().to_vec();
    bytes.push(recovery_id.to_byte());
    tx.delta_proof = Delta::Proof(DeltaProof::from_bytes(&bytes).unwrap());

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::DeltaMismatch) => {}
        other => panic!("a signature by another key must be a delta mismatch, got {other:?}"),
    }
}
