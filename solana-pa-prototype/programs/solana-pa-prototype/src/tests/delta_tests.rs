//! Curve point validation and arithmetic tests belong in arm_solana.

use crate::tests::utils::create_minimal_transaction;
use arm_core::delta_proof::DeltaWitness;
use arm_core::transaction::Delta;
use arm_solana::delta::verify_delta_proof;
use arm_solana::SolanaArmError;

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
fn test_verify_delta_proof_meaningless_proof_rejected() {
    // create_minimal_transaction carries a wire-valid but cryptographically
    // meaningless proof (r = s = 1) over a single action. The signature
    // recovers to *some* public key, which cannot match the accumulated
    // delta point, so verification fails at the key comparison. (With one
    // action no point addition runs; curve-arithmetic rejection is covered
    // by arm_solana's own test suite.)
    let tx = create_minimal_transaction();

    match verify_delta_proof(&tx) {
        Err(SolanaArmError::DeltaMismatch) => {}
        other => panic!(
            "Meaningless delta proof should fail the delta comparison, got {:?}",
            other
        ),
    }
}
