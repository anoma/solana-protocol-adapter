//! Property tests for Groth16 proof processing.

use proptest::prelude::*;
use crate::groth16::{negate_pi_a, extract_seal};

/// BN254 base field modulus (big-endian bytes) - copied for test verification.
const BN254_FIELD_MODULUS: [u8; 32] =
    hex_literal::hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: negate_pi_a is an involution (negate(negate(x)) = x).
    #[test]
    fn prop_negate_pi_a_involution(y_bytes in prop::array::uniform32(any::<u8>())) {
        // Create a valid seal structure (256 bytes)
        let mut seal = [0u8; 256];
        seal[32..64].copy_from_slice(&y_bytes);

        let negated = negate_pi_a(&seal);
        let double_negated = negate_pi_a(&negated);

        prop_assert_eq!(seal, double_negated, "double negation should return original");
    }

    /// Property: negate_pi_a(0) = 0 (zero y-coordinate stays zero).
    #[test]
    fn prop_negate_pi_a_zero(x_bytes in prop::array::uniform32(any::<u8>())) {
        let mut seal = [0u8; 256];
        seal[0..32].copy_from_slice(&x_bytes);
        // y is already zero

        let negated = negate_pi_a(&seal);

        // y-coordinate should still be zero
        prop_assert_eq!(&negated[32..64], &[0u8; 32], "negating zero y should stay zero");
        // x-coordinate should be unchanged
        prop_assert_eq!(&negated[0..32], &x_bytes, "x-coordinate should be unchanged");
    }

    /// Property: negate_pi_a only modifies y-coordinate (preserves x and rest of seal).
    #[test]
    fn prop_negate_pi_a_preserves_non_y(
        x_bytes in prop::array::uniform32(any::<u8>()),
        y_bytes in prop::array::uniform32(any::<u8>()),
        rest in prop::array::uniform(any::<u8>()).prop_map(|arr: [u8; 192]| arr),
    ) {
        let mut seal = [0u8; 256];
        seal[0..32].copy_from_slice(&x_bytes);
        seal[32..64].copy_from_slice(&y_bytes);
        seal[64..256].copy_from_slice(&rest);

        let negated = negate_pi_a(&seal);

        prop_assert_eq!(&negated[0..32], &x_bytes, "x-coordinate should be unchanged");
        prop_assert_eq!(&negated[64..256], &rest[..], "rest of seal should be unchanged");
    }

    /// Property: negation is field subtraction (-y = p - y mod BN254).
    #[test]
    fn prop_negate_is_field_subtraction(y_bytes in prop::array::uniform32(any::<u8>())) {
        let mut seal = [0u8; 256];
        seal[32..64].copy_from_slice(&y_bytes);

        let negated = negate_pi_a(&seal);
        let neg_y = &negated[32..64];

        // Convert to big integers and verify: y + neg_y = p (mod p) means neg_y = p - y
        let y = num_bigint::BigUint::from_bytes_be(&y_bytes);
        let neg = num_bigint::BigUint::from_bytes_be(neg_y);
        let p = num_bigint::BigUint::from_bytes_be(&BN254_FIELD_MODULUS);

        // For y < p: neg_y = p - y, so y + neg_y = p (which is 0 mod p)
        // For y >= p: the implementation does p - y which may underflow (undefined behavior in field)
        // but for well-formed inputs y < p, we verify: (y + neg_y) mod p == 0
        if y < p && !y.eq(&num_bigint::BigUint::ZERO) {
            let sum = (&y + &neg) % &p;
            prop_assert!(sum == num_bigint::BigUint::ZERO,
                "y + neg_y should be 0 mod p for valid field elements");
        }
    }

    /// Property: extract_seal requires at least 256 bytes.
    #[test]
    fn prop_extract_seal_length_check(len in 0usize..255) {
        let proof = vec![0u8; len];
        let result = extract_seal(&proof);
        prop_assert!(result.is_err(), "extract_seal should fail for proof < 256 bytes");
    }

    /// Property: extract_seal succeeds for proofs >= 256 bytes.
    #[test]
    fn prop_extract_seal_valid_length(extra in 0usize..100) {
        let proof = vec![0u8; 256 + extra];
        let result = extract_seal(&proof);
        prop_assert!(result.is_ok(), "extract_seal should succeed for proof >= 256 bytes");
        prop_assert_eq!(result.unwrap().len(), 256, "seal should be exactly 256 bytes");
    }
}
