//! Tests for Ed25519 signature verification parsing logic.
//!
//! These tests cover the pure parsing functions that don't require
//! mocking the instructions sysvar.

use crate::ed25519::{
    parse_ed25519_offsets, validate_ed25519_data, Ed25519Offsets, Ed25519ParseError,
    CURRENT_INSTRUCTION_INDEX, ED25519_MIN_INSTRUCTION_SIZE, SIGNATURE_OFFSETS_SERIALIZED_SIZE,
};

/// Helper to build a valid Ed25519 instruction data buffer.
/// Layout: [num_sigs, padding, sig_offset(2), sig_ix(2), pubkey_offset(2), pubkey_ix(2),
///          msg_offset(2), msg_size(2), msg_ix(2), ...data...]
fn build_ed25519_ix_data(
    num_signatures: u8,
    signature_offset: u16,
    pubkey_offset: u16,
    message_offset: u16,
    message_size: u16,
    pubkey: &[u8; 32],
    message: &[u8; 32],
) -> Vec<u8> {
    let mut data = vec![0u8; 16]; // header + offsets

    // Header
    data[0] = num_signatures;
    data[1] = 0; // padding

    // Offsets (all with 0xFFFF instruction index = current instruction)
    data[2..4].copy_from_slice(&signature_offset.to_le_bytes());
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes()); // sig_ix
    data[6..8].copy_from_slice(&pubkey_offset.to_le_bytes());
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes()); // pubkey_ix
    data[10..12].copy_from_slice(&message_offset.to_le_bytes());
    data[12..14].copy_from_slice(&message_size.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes()); // msg_ix

    // Pad to fit signature (64 bytes), pubkey (32 bytes), message (32 bytes)
    // Using offsets: signature at 16, pubkey at 80, message at 112
    let total_size = 16 + 64 + 32 + 32; // 144 bytes
    data.resize(total_size, 0);

    // Place pubkey and message at their offsets
    let pk_off = pubkey_offset as usize;
    let msg_off = message_offset as usize;
    if pk_off + 32 <= data.len() {
        data[pk_off..pk_off + 32].copy_from_slice(pubkey);
    }
    if msg_off + 32 <= data.len() {
        data[msg_off..msg_off + 32].copy_from_slice(message);
    }

    data
}

// =============================================================================
// Constants Tests
// =============================================================================

#[test]
fn test_constants() {
    assert_eq!(SIGNATURE_OFFSETS_SERIALIZED_SIZE, 14);
    assert_eq!(ED25519_MIN_INSTRUCTION_SIZE, 16);
    assert_eq!(CURRENT_INSTRUCTION_INDEX, 0xFFFF);
}

// =============================================================================
// parse_ed25519_offsets Tests
// =============================================================================

#[test]
fn test_parse_offsets_valid() {
    let pubkey = [1u8; 32];
    let message = [2u8; 32];
    let data = build_ed25519_ix_data(1, 16, 80, 112, 32, &pubkey, &message);

    let offsets = parse_ed25519_offsets(&data).unwrap();

    assert_eq!(offsets.num_signatures, 1);
    assert_eq!(offsets.signature_offset, 16);
    assert_eq!(offsets.signature_ix_index, CURRENT_INSTRUCTION_INDEX);
    assert_eq!(offsets.pubkey_offset, 80);
    assert_eq!(offsets.pubkey_ix_index, CURRENT_INSTRUCTION_INDEX);
    assert_eq!(offsets.message_offset, 112);
    assert_eq!(offsets.message_size, 32);
    assert_eq!(offsets.message_ix_index, CURRENT_INSTRUCTION_INDEX);
}

#[test]
fn test_parse_offsets_multiple_signatures() {
    let pubkey = [1u8; 32];
    let message = [2u8; 32];
    let mut data = build_ed25519_ix_data(3, 16, 80, 112, 32, &pubkey, &message);
    data[0] = 3; // 3 signatures

    // Should still parse (we only use first signature)
    let offsets = parse_ed25519_offsets(&data).unwrap();
    assert_eq!(offsets.num_signatures, 3);
}

#[test]
fn test_parse_offsets_instruction_too_short() {
    // Less than 16 bytes (minimum)
    let data = vec![1u8; 15];

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::InstructionTooShort));
}

#[test]
fn test_parse_offsets_empty() {
    let data: Vec<u8> = vec![];

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::InstructionTooShort));
}

#[test]
fn test_parse_offsets_exactly_minimum_size() {
    // Exactly 16 bytes - should parse header/offsets but data validation may fail later
    let mut data = vec![0u8; 16];
    data[0] = 1; // num_signatures
                 // Set all instruction indices to 0xFFFF
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());

    let result = parse_ed25519_offsets(&data);
    assert!(result.is_ok());
}

#[test]
fn test_parse_offsets_no_signatures() {
    let mut data = vec![0u8; 16];
    data[0] = 0; // num_signatures = 0

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::NoSignatures));
}

#[test]
fn test_parse_offsets_external_signature_reference() {
    let mut data = vec![0u8; 16];
    data[0] = 1; // num_signatures
                 // sig_ix = 0 (not 0xFFFF) - references another instruction
    data[4..6].copy_from_slice(&0u16.to_le_bytes());
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::ExternalDataReference));
}

#[test]
fn test_parse_offsets_external_pubkey_reference() {
    let mut data = vec![0u8; 16];
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    // pubkey_ix = 1 (not 0xFFFF)
    data[8..10].copy_from_slice(&1u16.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::ExternalDataReference));
}

#[test]
fn test_parse_offsets_external_message_reference() {
    let mut data = vec![0u8; 16];
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    // msg_ix = 2 (not 0xFFFF)
    data[14..16].copy_from_slice(&2u16.to_le_bytes());

    let result = parse_ed25519_offsets(&data);
    assert_eq!(result, Err(Ed25519ParseError::ExternalDataReference));
}

#[test]
fn test_parse_offsets_various_offset_values() {
    // Test that offset parsing correctly handles various u16 values
    let mut data = vec![0u8; 16];
    data[0] = 1;

    // Set some non-trivial offset values
    let sig_offset: u16 = 1234;
    let pk_offset: u16 = 5678;
    let msg_offset: u16 = 9012;
    let msg_size: u16 = 64;

    data[2..4].copy_from_slice(&sig_offset.to_le_bytes());
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&pk_offset.to_le_bytes());
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&msg_offset.to_le_bytes());
    data[12..14].copy_from_slice(&msg_size.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());

    let offsets = parse_ed25519_offsets(&data).unwrap();
    assert_eq!(offsets.signature_offset, 1234);
    assert_eq!(offsets.pubkey_offset, 5678);
    assert_eq!(offsets.message_offset, 9012);
    assert_eq!(offsets.message_size, 64);
}

// =============================================================================
// validate_ed25519_data Tests
// =============================================================================

#[test]
fn test_validate_data_valid() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];
    let data = build_ed25519_ix_data(1, 16, 80, 112, 32, &pubkey, &message);

    let offsets = parse_ed25519_offsets(&data).unwrap();
    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);

    assert!(result.is_ok());
}

#[test]
fn test_validate_data_pubkey_mismatch() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];
    let data = build_ed25519_ix_data(1, 16, 80, 112, 32, &pubkey, &message);

    let offsets = parse_ed25519_offsets(&data).unwrap();

    // Try to validate with a different pubkey
    let wrong_pubkey = [0xCC; 32];
    let result = validate_ed25519_data(&data, &offsets, &wrong_pubkey, &message);

    assert_eq!(result, Err(Ed25519ParseError::PubkeyMismatch));
}

#[test]
fn test_validate_data_message_mismatch() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];
    let data = build_ed25519_ix_data(1, 16, 80, 112, 32, &pubkey, &message);

    let offsets = parse_ed25519_offsets(&data).unwrap();

    // Try to validate with a different message
    let wrong_message = [0xDD; 32];
    let result = validate_ed25519_data(&data, &offsets, &pubkey, &wrong_message);

    assert_eq!(result, Err(Ed25519ParseError::MessageMismatch));
}

#[test]
fn test_validate_data_pubkey_out_of_bounds() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    // Create data large enough for signature (at 0, needs 64 bytes) but with pubkey out of bounds
    // Signature at 0, needs 64 bytes → data must be >= 64
    // Pubkey at 80, needs 112 bytes → out of bounds in 80-byte buffer
    let mut data = vec![0u8; 80];
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&80u16.to_le_bytes()); // pubkey at 80 - out of bounds!
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&16u16.to_le_bytes()); // message at 16
    data[12..14].copy_from_slice(&32u16.to_le_bytes()); // message size
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[16..48].copy_from_slice(&message);

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 0, // Signature at start (fits in 80 bytes)
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 80, // Out of bounds!
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 16,
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::PubkeyOutOfBounds));
}

#[test]
fn test_validate_data_message_out_of_bounds() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    // Create data with message offset pointing beyond end
    let mut data = vec![0u8; 80]; // pubkey fits at 16-48, but message at 112 doesn't fit
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&16u16.to_le_bytes()); // pubkey at 16
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&112u16.to_le_bytes()); // message at 112 - out of bounds
    data[12..14].copy_from_slice(&32u16.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[16..48].copy_from_slice(&pubkey);

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 16,
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 16,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 112, // Out of bounds!
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::MessageOutOfBounds));
}

#[test]
fn test_validate_data_invalid_message_size() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];
    let mut data = build_ed25519_ix_data(1, 16, 80, 112, 32, &pubkey, &message);

    // Extend data to accommodate a 64-byte message (bounds check passes, size check fails)
    data.resize(112 + 64, 0); // message_offset + 64 = 176 bytes

    // Modify message_size in the data header
    data[12..14].copy_from_slice(&64u16.to_le_bytes()); // 64 instead of 32

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 16,
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 80,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 112,
        message_size: 64, // Wrong size - should be 32
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::InvalidMessageSize));
}

#[test]
fn test_validate_data_zero_message_size() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 16,
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 16,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 48,
        message_size: 0, // Zero size - invalid
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let data = build_ed25519_ix_data(1, 16, 16, 48, 0, &pubkey, &message);
    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::InvalidMessageSize));
}

// =============================================================================
// Edge Cases and Security Tests
// =============================================================================

#[test]
fn test_boundary_pubkey_offset_at_exact_end() {
    // pubkey_offset + 32 == data.len() (exactly at boundary - should pass)
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    // Build data where pubkey ends exactly at data.len()
    let mut data = vec![0u8; 16 + 64 + 32]; // 112 bytes: header + sig + pubkey
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&80u16.to_le_bytes()); // pubkey at 80, ends at 112
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&16u16.to_le_bytes()); // message at 16
    data[12..14].copy_from_slice(&32u16.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[80..112].copy_from_slice(&pubkey);
    data[16..48].copy_from_slice(&message);

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 16,
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 80,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 16,
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    // Should succeed - pubkey fits exactly
    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert!(result.is_ok());
}

#[test]
fn test_boundary_pubkey_offset_one_byte_over() {
    // pubkey_offset + 32 == data.len() + 1 (one byte over - should fail)
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    let data = vec![0u8; 111]; // One byte less than needed

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 16,
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 80, // 80 + 32 = 112, but data.len() = 111
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 16,
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::PubkeyOutOfBounds));
}

#[test]
fn test_max_u16_offset_values() {
    // Test with maximum u16 values to ensure no overflow issues
    let mut data = vec![0u8; 16];
    data[0] = 1;

    // Use max u16 for offsets (except instruction indices which must be 0xFFFF)
    data[2..4].copy_from_slice(&u16::MAX.to_le_bytes()); // sig_offset
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&u16::MAX.to_le_bytes()); // pubkey_offset
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&u16::MAX.to_le_bytes()); // msg_offset
    data[12..14].copy_from_slice(&u16::MAX.to_le_bytes()); // msg_size
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());

    // Should parse without overflow
    let offsets = parse_ed25519_offsets(&data).unwrap();
    assert_eq!(offsets.signature_offset, u16::MAX as usize);
    assert_eq!(offsets.pubkey_offset, u16::MAX as usize);
    assert_eq!(offsets.message_offset, u16::MAX as usize);
    assert_eq!(offsets.message_size, u16::MAX as usize);
}

#[test]
fn test_overlapping_pubkey_and_message() {
    // Pubkey and message can technically overlap in the data
    // This tests that we correctly read from the specified offsets
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    // Both at same offset - should fail because data won't match both
    // Signature at 0 fits in 64 bytes, pubkey and message at 64 overlap
    let mut data = vec![0u8; 96]; // Large enough for signature (64) + pubkey/message area
    data[0] = 1;
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&64u16.to_le_bytes()); // pubkey at 64
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&64u16.to_le_bytes()); // message also at 64!
    data[12..14].copy_from_slice(&32u16.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[64..96].copy_from_slice(&pubkey); // Put pubkey data there

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 0, // Signature at start (fits in 64 bytes)
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 64,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 64, // Same as pubkey!
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    // Should fail because the data at offset 64 can't match both pubkey and message
    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::MessageMismatch));
}

#[test]
fn test_validate_data_signature_out_of_bounds() {
    let pubkey = [0xAA; 32];
    let message = [0xBB; 32];

    // Create data with signature offset pointing beyond end
    let mut data = vec![0u8; 80]; // Too small for signature at offset 100
    data[0] = 1;
    data[2..4].copy_from_slice(&100u16.to_le_bytes()); // signature at 100 - out of bounds
    data[4..6].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[6..8].copy_from_slice(&16u16.to_le_bytes()); // pubkey at 16
    data[8..10].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[10..12].copy_from_slice(&48u16.to_le_bytes()); // message at 48
    data[12..14].copy_from_slice(&32u16.to_le_bytes());
    data[14..16].copy_from_slice(&CURRENT_INSTRUCTION_INDEX.to_le_bytes());
    data[16..48].copy_from_slice(&pubkey);
    data[48..80].copy_from_slice(&message);

    let offsets = Ed25519Offsets {
        num_signatures: 1,
        signature_offset: 100, // Out of bounds!
        signature_ix_index: CURRENT_INSTRUCTION_INDEX,
        pubkey_offset: 16,
        pubkey_ix_index: CURRENT_INSTRUCTION_INDEX,
        message_offset: 48,
        message_size: 32,
        message_ix_index: CURRENT_INSTRUCTION_INDEX,
    };

    let result = validate_ed25519_data(&data, &offsets, &pubkey, &message);
    assert_eq!(result, Err(Ed25519ParseError::SignatureOutOfBounds));
}
