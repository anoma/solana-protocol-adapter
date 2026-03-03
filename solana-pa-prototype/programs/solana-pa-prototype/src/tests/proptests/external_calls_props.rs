//! Property tests for external call encoding/decoding.

use super::strategies::{arb_byte_vec, arb_solana_external_call};
use crate::external_calls::{
    build_forwarder_instruction_data, decode_external_call, encode_external_call, verify_output,
    FORWARD_CALL_DISCRIMINATOR,
};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: encode followed by decode should return the original call (roundtrip identity).
    #[test]
    fn prop_external_call_encode_decode_roundtrip(call in arb_solana_external_call(256)) {
        let blob = encode_external_call(&call);
        let decoded = decode_external_call(&blob).expect("decode should succeed for valid encoded blob");
        prop_assert_eq!(call, decoded, "roundtrip should preserve external call");
    }

    /// Property: verify_output succeeds when expected equals actual.
    #[test]
    fn prop_verify_output_equal_succeeds(
        data in arb_byte_vec(512),
    ) {
        let result = verify_output(&data, &data);
        prop_assert!(result.is_ok(), "verify_output should succeed when expected == actual");
    }

    /// Property: verify_output fails when expected differs from actual (same length).
    #[test]
    fn prop_verify_output_mismatch_fails(
        expected in arb_byte_vec(512).prop_filter("non-empty", |v| !v.is_empty()),
    ) {
        let mut actual = expected.clone();
        actual[0] = actual[0].wrapping_add(1);

        let result = verify_output(&expected, &actual);
        prop_assert!(result.is_err(), "verify_output should fail when expected != actual");
    }

    /// Property: verify_output fails when expected and actual have different lengths.
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

    /// Property: build_forwarder_instruction_data produces correct layout:
    /// - First 8 bytes: discriminator
    /// - Next 32 bytes: logic_ref
    /// - Next 4 bytes: input length (u32 LE)
    /// - Remaining bytes: input data
    #[test]
    fn prop_forwarder_ix_data_format(
        logic_ref in prop::array::uniform32(any::<u8>()),
        input in arb_byte_vec(256),
    ) {
        let ix_data = build_forwarder_instruction_data(&logic_ref, &input);

        // Check total length: 8 + 32 + 4 + input.len()
        let expected_len = 8 + 32 + 4 + input.len();
        prop_assert_eq!(
            ix_data.len(),
            expected_len,
            "instruction data length should be 8 + 32 + 4 + input.len()"
        );

        // Check discriminator (bytes 0..8)
        prop_assert_eq!(
            &ix_data[0..8],
            &FORWARD_CALL_DISCRIMINATOR,
            "discriminator should match FORWARD_CALL_DISCRIMINATOR"
        );

        // Check logic_ref (bytes 8..40)
        prop_assert_eq!(
            &ix_data[8..40],
            &logic_ref,
            "logic_ref should be at bytes 8..40"
        );

        // Check input length (bytes 40..44, u32 LE)
        let input_len_bytes: [u8; 4] = ix_data[40..44].try_into().expect("4 bytes");
        let stored_input_len = u32::from_le_bytes(input_len_bytes);
        prop_assert_eq!(
            stored_input_len as usize,
            input.len(),
            "stored input length should match actual input length"
        );

        // Check input data (bytes 44..)
        prop_assert_eq!(
            &ix_data[44..],
            input.as_slice(),
            "input data should follow the length field"
        );
    }
}
