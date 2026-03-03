//! Unit tests for delta proof verification and curve point validation.

use arm_solana::SolanaArmError;

use crate::encoding::bytes_to_words;
use crate::tests::utils::{build_tx_from_instances, create_compliance_instance};
use crate::types::{Digest, Transaction};
use arm_solana::delta::{accumulate_deltas, collect_tags, compute_verifying_key};

/// secp256k1 generator point G (big-endian SEC1 format).
const G_X_BE: [u8; 32] =
    hex_literal::hex!("79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798");
const G_Y_BE: [u8; 32] =
    hex_literal::hex!("483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8");

/// Convert big-endian bytes to [u32; 8] words, using the shared `bytes_to_words`.
fn be_bytes_to_word_array(bytes: &[u8; 32]) -> [u32; 8] {
    bytes_to_words(bytes).try_into().unwrap()
}

/// Build a transaction with given delta values for testing.
fn build_tx_with_delta(delta_x: [u32; 8], delta_y: [u32; 8]) -> Transaction {
    let nf = Digest::from_bytes([1u8; 32]);
    let cm = Digest::from_bytes([2u8; 32]);
    let mut instance = create_compliance_instance(nf, cm);
    instance.delta_x = delta_x;
    instance.delta_y = delta_y;
    build_tx_from_instances(&[instance])
}

/// Build a transaction with two compliance units having given deltas.
fn build_tx_with_two_deltas(
    delta1_x: [u32; 8],
    delta1_y: [u32; 8],
    delta2_x: [u32; 8],
    delta2_y: [u32; 8],
) -> Transaction {
    let mut i1 =
        create_compliance_instance(Digest::from_bytes([1u8; 32]), Digest::from_bytes([2u8; 32]));
    i1.delta_x = delta1_x;
    i1.delta_y = delta1_y;

    let mut i2 =
        create_compliance_instance(Digest::from_bytes([3u8; 32]), Digest::from_bytes([4u8; 32]));
    i2.delta_x = delta2_x;
    i2.delta_y = delta2_y;

    build_tx_from_instances(&[i1, i2])
}

/// Compute -G (negation of generator point).
fn compute_neg_g() -> ([u32; 8], [u32; 8]) {
    use num_bigint::BigUint;
    use num_traits::Num;

    let p = BigUint::from_str_radix(
        "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F",
        16,
    )
    .unwrap();
    let gy = BigUint::from_bytes_be(&G_Y_BE);
    let neg_gy = &p - &gy;

    let mut neg_gy_bytes = [0u8; 32];
    let neg_gy_vec = neg_gy.to_bytes_be();
    neg_gy_bytes[32 - neg_gy_vec.len()..].copy_from_slice(&neg_gy_vec);

    (
        be_bytes_to_word_array(&G_X_BE),
        be_bytes_to_word_array(&neg_gy_bytes),
    )
}

#[test]
fn test_valid_generator_point_passes_validation() {
    // Use the secp256k1 generator point G - a known valid curve point
    let delta_x = be_bytes_to_word_array(&G_X_BE);
    let delta_y = be_bytes_to_word_array(&G_Y_BE);

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(
        result.is_ok(),
        "Generator point G should be valid: {:?}",
        result.err()
    );

    let point = result.unwrap();
    assert!(point.is_some(), "Should return a point, not identity");
}

#[test]
fn test_zero_point_rejected() {
    // (0, 0) is NOT on the secp256k1 curve - the identity has no affine representation
    let delta_x = [0u32; 8];
    let delta_y = [0u32; 8];

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(result.is_err(), "(0, 0) should be rejected as invalid");
    assert!(
        matches!(result.err(), Some(SolanaArmError::DeltaPointNotOnCurve)),
        "Should return DeltaPointNotOnCurve error"
    );
}

#[test]
fn test_wrong_y_coordinate_rejected() {
    // Valid x (generator), but wrong y (all zeros instead of actual y)
    let delta_x = be_bytes_to_word_array(&G_X_BE);
    let delta_y = [0u32; 8]; // Wrong y - should be G_Y

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(result.is_err(), "Valid x with wrong y should be rejected");
    assert!(
        matches!(result.err(), Some(SolanaArmError::DeltaPointNotOnCurve)),
        "Should return DeltaPointNotOnCurve error"
    );
}

#[test]
fn test_invalid_x_rejected() {
    // Invalid x coordinate (all 1s is unlikely to be on curve)
    let delta_x = [1u32; 8];
    let delta_y = [1u32; 8];

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    // This should fail - random coordinates aren't valid curve points
    assert!(
        result.is_err(),
        "Random coordinates should be rejected as not on curve"
    );
}

#[test]
fn test_negated_y_is_valid() {
    // The negation of G is also a valid point: (Gx, -Gy mod p)
    let (delta_x, delta_y) = compute_neg_g();

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(
        result.is_ok(),
        "Negated generator point -G should be valid: {:?}",
        result.err()
    );
}

#[test]
fn test_collect_tags_with_valid_delta() {
    let delta_x = be_bytes_to_word_array(&G_X_BE);
    let delta_y = be_bytes_to_word_array(&G_Y_BE);

    let tx = build_tx_with_delta(delta_x, delta_y);
    let tags = collect_tags(&tx);

    assert_eq!(tags.len(), 2, "Should have 2 tags (nf, cm) for 1 CU");
}

#[test]
fn test_verifying_key_deterministic() {
    let delta_x = be_bytes_to_word_array(&G_X_BE);
    let delta_y = be_bytes_to_word_array(&G_Y_BE);

    let tx = build_tx_with_delta(delta_x, delta_y);
    let tags = collect_tags(&tx);

    let vk1 = compute_verifying_key(&tags);
    let vk2 = compute_verifying_key(&tags);

    assert_eq!(vk1, vk2, "Same tags should produce same verifying key");
}

#[test]
fn test_point_doubling_g_plus_g() {
    // G + G = 2G (point doubling case in safe_point_add)
    let delta_x = be_bytes_to_word_array(&G_X_BE);
    let delta_y = be_bytes_to_word_array(&G_Y_BE);

    let tx = build_tx_with_two_deltas(delta_x, delta_y, delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(result.is_ok(), "G + G should succeed: {:?}", result.err());

    let point = result.unwrap();
    assert!(point.is_some(), "G + G = 2G, not identity");

    // Verify it's different from G (it's 2G)
    let two_g = point.unwrap();
    assert_ne!(two_g.0[..32], G_X_BE, "2G should have different x than G");
}

#[test]
fn test_inverse_points_g_plus_neg_g_equals_identity() {
    // G + (-G) = identity (inverse points case in safe_point_add)
    let (g_x, g_y) = (
        be_bytes_to_word_array(&G_X_BE),
        be_bytes_to_word_array(&G_Y_BE),
    );
    let (neg_g_x, neg_g_y) = compute_neg_g();

    let tx = build_tx_with_two_deltas(g_x, g_y, neg_g_x, neg_g_y);
    let result = accumulate_deltas(&tx);

    assert!(
        result.is_ok(),
        "G + (-G) should succeed: {:?}",
        result.err()
    );

    let point = result.unwrap();
    assert!(point.is_none(), "G + (-G) should equal identity (None)");
}
