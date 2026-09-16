//! Tests for the Ed25519 instruction parsing, which needs no sysvar.

use crate::ed25519::{
    parse_ed25519_offsets, validate_ed25519_data, Ed25519Offsets, CURRENT_INSTRUCTION_INDEX,
    ED25519_MIN_INSTRUCTION_SIZE,
};
use crate::ErrorCode;

const PUBKEY: [u8; 32] = [0xAA; 32];
const MESSAGE: [u8; 32] = [0xBB; 32];

/// An Ed25519 instruction with one signature: header and offsets, then the
/// signature at 16, the public key at 80 and the message at 112 (144 bytes).
fn ed25519_ix_data(signature_offset: u16, pubkey_offset: u16, message_offset: u16) -> Vec<u8> {
    let mut data = vec![0u8; 144];
    data[0] = 1;
    for (i, field) in [
        signature_offset,
        CURRENT_INSTRUCTION_INDEX,
        pubkey_offset,
        CURRENT_INSTRUCTION_INDEX,
        message_offset,
        MESSAGE.len() as u16,
        CURRENT_INSTRUCTION_INDEX,
    ]
    .iter()
    .enumerate()
    {
        data[2 + 2 * i..4 + 2 * i].copy_from_slice(&field.to_le_bytes());
    }
    data[80..112].copy_from_slice(&PUBKEY);
    data[112..144].copy_from_slice(&MESSAGE);
    data
}

fn valid_ix_data() -> Vec<u8> {
    ed25519_ix_data(16, 80, 112)
}

#[test]
fn parses_the_first_signature_offsets() {
    assert_eq!(
        parse_ed25519_offsets(&valid_ix_data()).unwrap(),
        Ed25519Offsets {
            pubkey_offset: 80,
            message_offset: 112,
            message_size: 32,
        }
    );
}

#[test]
fn rejects_data_shorter_than_the_header_and_one_offset_block() {
    assert!(matches!(
        parse_ed25519_offsets(&[]),
        Err(ErrorCode::InvalidEd25519Instruction)
    ));
    assert!(matches!(
        parse_ed25519_offsets(&[1u8; ED25519_MIN_INSTRUCTION_SIZE - 1]),
        Err(ErrorCode::InvalidEd25519Instruction)
    ));
    assert!(parse_ed25519_offsets(&valid_ix_data()[..ED25519_MIN_INSTRUCTION_SIZE]).is_ok());
}

#[test]
fn rejects_zero_signatures() {
    let mut data = valid_ix_data();
    data[0] = 0;
    assert!(matches!(
        parse_ed25519_offsets(&data),
        Err(ErrorCode::InvalidEd25519Instruction)
    ));
}

#[test]
fn rejects_data_referenced_from_another_instruction() {
    // The signature, public key and message instruction indices, in turn.
    for index_field in [4usize, 8, 14] {
        let mut data = valid_ix_data();
        data[index_field..index_field + 2].copy_from_slice(&0u16.to_le_bytes());
        assert!(
            matches!(
                parse_ed25519_offsets(&data),
                Err(ErrorCode::InvalidEd25519Instruction)
            ),
            "index field at {index_field} must be the current instruction"
        );
    }
}

#[test]
fn validates_the_expected_pubkey_and_message() {
    let data = valid_ix_data();
    let offsets = parse_ed25519_offsets(&data).unwrap();
    assert!(validate_ed25519_data(&data, &offsets, &PUBKEY, &MESSAGE).is_ok());
    assert!(matches!(
        validate_ed25519_data(&data, &offsets, &[0xCC; 32], &MESSAGE),
        Err(ErrorCode::Ed25519PubkeyMismatch)
    ));
    assert!(matches!(
        validate_ed25519_data(&data, &offsets, &PUBKEY, &[0xDD; 32]),
        Err(ErrorCode::Ed25519MessageMismatch)
    ));
    assert!(
        matches!(
            validate_ed25519_data(&data, &offsets, &PUBKEY, &MESSAGE[..31]),
            Err(ErrorCode::Ed25519MessageMismatch)
        ),
        "a message of another length is a mismatch, whatever its prefix"
    );
}

#[test]
fn rejects_offsets_past_the_end_of_the_data() {
    let data = valid_ix_data();
    for (name, offsets) in [
        (
            "pubkey",
            Ed25519Offsets {
                pubkey_offset: 144 - 31,
                message_offset: 112,
                message_size: 32,
            },
        ),
        (
            "message",
            Ed25519Offsets {
                pubkey_offset: 80,
                message_offset: 113,
                message_size: 32,
            },
        ),
        (
            "u16 maximum",
            Ed25519Offsets {
                pubkey_offset: u16::MAX as usize,
                message_offset: u16::MAX as usize,
                message_size: u16::MAX as usize,
            },
        ),
    ] {
        assert!(
            matches!(
                validate_ed25519_data(&data, &offsets, &PUBKEY, &MESSAGE),
                Err(ErrorCode::InvalidEd25519Instruction)
            ),
            "{name} out of bounds must be rejected"
        );
    }
}
