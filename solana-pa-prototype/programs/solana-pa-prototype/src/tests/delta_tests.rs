//! Unit tests for delta proof verification and curve point validation.

use crate::delta::{accumulate_deltas, collect_tags, compute_verifying_key};
use crate::error::PAError;
use crate::tests::utils::create_compliance_instance;
use crate::types::{Action, ComplianceUnit, Delta, LogicVerifierInputs, Transaction};

/// Convert big-endian bytes to [u32; 8] words using arm-risc0's encoding.
///
/// This matches arm-risc0/arm/src/utils.rs bytes_to_words:
/// - Takes big-endian bytes
/// - Builds each word from 4 bytes in big-endian order
/// - Stores as u32::from_be(word) which on little-endian gives native storage
fn bytes_to_words_be(bytes: &[u8; 32]) -> [u32; 8] {
    let mut words = [0u32; 8];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        let word = ((chunk[0] as u32) << 24)
            | ((chunk[1] as u32) << 16)
            | ((chunk[2] as u32) << 8)
            | (chunk[3] as u32);
        words[i] = u32::from_be(word);
    }
    words
}

/// secp256k1 generator point G (big-endian SEC1 format).
const G_X_BE: [u8; 32] =
    hex_literal::hex!("79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798");
const G_Y_BE: [u8; 32] =
    hex_literal::hex!("483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8");

/// Build a transaction from a single compliance instance for testing.
fn build_tx_with_delta(delta_x: [u32; 8], delta_y: [u32; 8]) -> Transaction {
    use crate::types::Digest;
    let nf = Digest::from_bytes([1u8; 32]);
    let cm = Digest::from_bytes([2u8; 32]);
    let mut instance = create_compliance_instance(nf, cm);
    instance.delta_x = delta_x;
    instance.delta_y = delta_y;

    Transaction {
        actions: vec![Action {
            compliance_units: vec![ComplianceUnit {
                instance: instance.clone(),
                proof: None,
            }],
            logic_verifier_inputs: vec![
                LogicVerifierInputs {
                    tag: instance.consumed_nullifier,
                    verifying_key: instance.consumed_logic_ref,
                    app_data: Default::default(),
                    proof: None,
                    instance_journal: Vec::new(),
                },
                LogicVerifierInputs {
                    tag: instance.created_commitment,
                    verifying_key: instance.created_logic_ref,
                    app_data: Default::default(),
                    proof: None,
                    instance_journal: Vec::new(),
                },
            ],
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
}

#[test]
fn test_valid_generator_point_passes_validation() {
    // Use the secp256k1 generator point G - a known valid curve point
    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = bytes_to_words_be(&G_Y_BE);

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
        matches!(result.err(), Some(PAError::DeltaPointNotOnCurve)),
        "Should return DeltaPointNotOnCurve error"
    );
}

#[test]
fn test_wrong_y_coordinate_rejected() {
    // Valid x (generator), but wrong y (all zeros instead of actual y)
    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = [0u32; 8]; // Wrong y - should be G_Y

    let tx = build_tx_with_delta(delta_x, delta_y);
    let result = accumulate_deltas(&tx);

    assert!(result.is_err(), "Valid x with wrong y should be rejected");
    assert!(
        matches!(result.err(), Some(PAError::DeltaPointNotOnCurve)),
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
    // secp256k1 p = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
    // -Gy = p - Gy

    // Compute -Gy = p - Gy in big-endian
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
    // Pad with leading zeros if needed
    neg_gy_bytes[32 - neg_gy_vec.len()..].copy_from_slice(&neg_gy_vec);

    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = bytes_to_words_be(&neg_gy_bytes);

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
    // collect_tags doesn't validate deltas, so this should work
    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = bytes_to_words_be(&G_Y_BE);

    let tx = build_tx_with_delta(delta_x, delta_y);
    let tags = collect_tags(&tx);

    assert!(tags.is_ok());
    let tags = tags.unwrap();
    assert_eq!(tags.len(), 2, "Should have 2 tags (nf, cm) for 1 CU");
}

#[test]
fn test_verifying_key_deterministic() {
    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = bytes_to_words_be(&G_Y_BE);

    let tx = build_tx_with_delta(delta_x, delta_y);
    let tags = collect_tags(&tx).unwrap();

    let vk1 = compute_verifying_key(&tags);
    let vk2 = compute_verifying_key(&tags);

    assert_eq!(vk1, vk2, "Same tags should produce same verifying key");
}

/// Build a transaction with two compliance units having given deltas.
fn build_tx_with_two_deltas(
    delta1_x: [u32; 8],
    delta1_y: [u32; 8],
    delta2_x: [u32; 8],
    delta2_y: [u32; 8],
) -> Transaction {
    use crate::types::{ComplianceInstance, Digest};

    let nf1 = Digest::from_bytes([1u8; 32]);
    let cm1 = Digest::from_bytes([2u8; 32]);
    let nf2 = Digest::from_bytes([3u8; 32]);
    let cm2 = Digest::from_bytes([4u8; 32]);

    let instance1 = ComplianceInstance {
        consumed_nullifier: nf1,
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: Digest::default(),
        created_commitment: cm1,
        created_logic_ref: Digest::default(),
        delta_x: delta1_x,
        delta_y: delta1_y,
    };

    let instance2 = ComplianceInstance {
        consumed_nullifier: nf2,
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: Digest::default(),
        created_commitment: cm2,
        created_logic_ref: Digest::default(),
        delta_x: delta2_x,
        delta_y: delta2_y,
    };

    Transaction {
        actions: vec![Action {
            compliance_units: vec![
                ComplianceUnit {
                    instance: instance1,
                    proof: None,
                },
                ComplianceUnit {
                    instance: instance2,
                    proof: None,
                },
            ],
            logic_verifier_inputs: vec![],
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
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

    (bytes_to_words_be(&G_X_BE), bytes_to_words_be(&neg_gy_bytes))
}

#[test]
fn test_point_doubling_g_plus_g() {
    // G + G = 2G (point doubling case in safe_point_add)
    let delta_x = bytes_to_words_be(&G_X_BE);
    let delta_y = bytes_to_words_be(&G_Y_BE);

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
    let (g_x, g_y) = (bytes_to_words_be(&G_X_BE), bytes_to_words_be(&G_Y_BE));
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
