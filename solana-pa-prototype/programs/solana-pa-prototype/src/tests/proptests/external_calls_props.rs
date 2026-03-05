use super::strategies::{arb_byte_vec, arb_solana_external_call};
use crate::external_calls::{decode_external_call, encode_external_call, verify_output};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_external_call_encode_decode_roundtrip(call in arb_solana_external_call(256)) {
        let blob = encode_external_call(&call);
        let decoded = decode_external_call(&blob).expect("decode should succeed for valid encoded blob");
        prop_assert_eq!(call, decoded, "roundtrip should preserve external call");
    }

    #[test]
    fn prop_verify_output_mismatch_fails(
        expected in arb_byte_vec(512).prop_filter("non-empty", |v| !v.is_empty()),
    ) {
        let mut actual = expected.clone();
        actual[0] = actual[0].wrapping_add(1);

        let result = verify_output(&expected, &actual);
        prop_assert!(result.is_err(), "verify_output should fail when expected != actual");
    }

    #[test]
    fn prop_verify_output_length_mismatch_fails(
        expected in arb_byte_vec(512),
        extra_byte in any::<u8>(),
    ) {
        let mut actual = expected.clone();
        actual.push(extra_byte);

        let result = verify_output(&expected, &actual);
        prop_assert!(result.is_err(), "verify_output should fail when lengths differ");
    }
}
