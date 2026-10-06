use crate::error::PAError;
use crate::groth16::{negate_g1, prepare_proof_for_verification};
use crate::tests::utils::{fake_aggregation_proof_bytes, minimal_instance, FAKE_SELECTOR};
use arm_core::constants::BATCH_AGGREGATION_VK;
use arm_core::transaction::Aggregation;
use hex_literal::hex;

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
        Err(crate::error::PAError::RiscZeroVerifierSelectorMismatch) => {}
        Err(other) => panic!("Expected RiscZeroVerifierSelectorMismatch, got {:?}", other),
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

/// pi_a of the seal in the real-mode fixture tests/fixtures/batch_groth16.json
/// (a Groth16 proof from the RISC Zero prover; the 64 bytes after the
/// 0x73c457ba selector).
const FIXTURE_PI_A: [u8; 64] = hex!(
    "29f13127e4ac4620c2de88226d7495ef36a16f9676f5a3429aed66210b201fdb"
    "2a398b5f7ffba002c5c874c657bf7d8cc3f97ae31c13cc31194dc210ab868fb8"
);

/// `groth_16_verifier::negate_g1(&FIXTURE_PI_A)` from risc0-solana v3.0.0
/// (commit ee415935), the negation the deployed verifier stack is built for.
const FIXTURE_PI_A_NEGATED: [u8; 64] = hex!(
    "29f13127e4ac4620c2de88226d7495ef36a16f9676f5a3429aed66210b201fdb"
    "062ac31361360026f287d0f029c1dad0d387efae4c5dfe5c22d2ca062cf66d8f"
);

/// BN254 base-field modulus q, big-endian.
const BASE_FIELD_MODULUS: [u8; 32] =
    hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");

#[test]
fn test_negate_g1_matches_risc0_solana_on_fixture_pi_a() {
    assert_eq!(
        negate_g1(&FIXTURE_PI_A).unwrap(),
        FIXTURE_PI_A_NEGATED,
        "negation of a real proof's pi_a must equal what the risc0-solana verifier crate produced"
    );
}

#[test]
fn test_negate_g1_is_the_group_inverse() {
    let negated = negate_g1(&FIXTURE_PI_A).unwrap();
    let sum = solana_bn254::prelude::alt_bn128_g1_addition_be(&[FIXTURE_PI_A, negated].concat())
        .expect("both points must be valid G1 points for the alt_bn128 addition");
    assert_eq!(
        sum,
        vec![0u8; 64],
        "P + negate(P) must be the point at infinity (all-zero encoding)"
    );
}

/// Mock seals carry an all-zero pi_a; the negation must keep it the point
/// at infinity rather than failing.
#[test]
fn test_negate_g1_point_at_infinity_is_fixed() {
    assert_eq!(negate_g1(&[0u8; 64]).unwrap(), [0u8; 64]);
}

#[test]
fn test_negate_g1_rejects_non_canonical_coordinates() {
    let mut y_is_modulus = FIXTURE_PI_A;
    y_is_modulus[32..].copy_from_slice(&BASE_FIELD_MODULUS);
    assert!(
        matches!(negate_g1(&y_is_modulus), Err(PAError::InvalidProof)),
        "a coordinate equal to the field modulus is not a canonical field element"
    );

    let mut x_is_modulus = FIXTURE_PI_A;
    x_is_modulus[..32].copy_from_slice(&BASE_FIELD_MODULUS);
    assert!(
        matches!(negate_g1(&x_is_modulus), Err(PAError::InvalidProof)),
        "a coordinate equal to the field modulus is not a canonical field element"
    );
}
