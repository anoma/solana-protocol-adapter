//! Ed25519 signature verification via instruction introspection.
//!
//! Solana's Ed25519 program is a precompile that verifies signatures.
//! Our program cannot call it directly via CPI. Instead:
//! 1. The transaction includes an Ed25519 verify instruction
//! 2. Our program uses instruction introspection to confirm it exists
//! 3. We verify the pubkey and message in that instruction match expectations
//!
//! Reference: https://rareskills.io/post/solana-signature-verification

use anchor_lang::prelude::*;
use anchor_lang::solana_program::ed25519_program;
use anchor_lang::solana_program::sysvar::instructions::{
    load_instruction_at_checked, ID as IX_SYSVAR_ID,
};

use crate::ErrorCode;

/// Ed25519 signature data offsets in instruction data.
///
/// The Ed25519 instruction data format is:
/// - num_signatures: u8 (byte 0)
/// - padding: u8 (byte 1)
/// - For each signature:
///   - signature_offset: u16 (offset within instruction data or another instruction)
///   - signature_instruction_index: u16 (0xFFFF = current instruction)
///   - public_key_offset: u16
///   - public_key_instruction_index: u16
///   - message_data_offset: u16
///   - message_data_size: u16
///   - message_instruction_index: u16
/// - Then the actual data (signature, pubkey, message) at the specified offsets
pub const SIGNATURE_OFFSETS_SERIALIZED_SIZE: usize = 14; // 7 x u16

/// Minimum size for a valid Ed25519 instruction: 2-byte header + one signature's offsets
pub const ED25519_MIN_INSTRUCTION_SIZE: usize = 2 + SIGNATURE_OFFSETS_SERIALIZED_SIZE;

/// The instruction index value meaning "data is in current instruction"
pub const CURRENT_INSTRUCTION_INDEX: u16 = 0xFFFF;

/// Parsed offsets from an Ed25519 instruction.
/// All offsets are relative to the instruction data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed25519Offsets {
    pub num_signatures: u8,
    pub signature_offset: usize,
    pub signature_ix_index: u16,
    pub pubkey_offset: usize,
    pub pubkey_ix_index: u16,
    pub message_offset: usize,
    pub message_size: usize,
    pub message_ix_index: u16,
}

/// Error types for Ed25519 parsing (for unit tests that don't have access to ErrorCode)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ed25519ParseError {
    /// Instruction data is too short
    InstructionTooShort,
    /// No signatures in instruction
    NoSignatures,
    /// Data references external instruction (not 0xFFFF)
    ExternalDataReference,
    /// Signature data is out of bounds
    SignatureOutOfBounds,
    /// Pubkey data is out of bounds
    PubkeyOutOfBounds,
    /// Message data is out of bounds
    MessageOutOfBounds,
    /// Message size doesn't match expected wrap message length
    InvalidMessageSize,
    /// Pubkey doesn't match expected
    PubkeyMismatch,
    /// Message doesn't match expected
    MessageMismatch,
}

impl From<Ed25519ParseError> for ErrorCode {
    fn from(e: Ed25519ParseError) -> Self {
        match e {
            Ed25519ParseError::InstructionTooShort => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::NoSignatures => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::ExternalDataReference => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::SignatureOutOfBounds => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::PubkeyOutOfBounds => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::MessageOutOfBounds => ErrorCode::InvalidEd25519Instruction,
            Ed25519ParseError::InvalidMessageSize => ErrorCode::Ed25519MessageMismatch,
            Ed25519ParseError::PubkeyMismatch => ErrorCode::Ed25519PubkeyMismatch,
            Ed25519ParseError::MessageMismatch => ErrorCode::Ed25519MessageMismatch,
        }
    }
}

/// Parse Ed25519 instruction data into structured offsets.
///
/// This is a pure function that can be unit tested without mocking sysvars.
///
/// # Arguments
/// * `ix_data` - Raw instruction data bytes
///
/// # Returns
/// Parsed offsets or an error if the data is malformed.
pub fn parse_ed25519_offsets(
    ix_data: &[u8],
) -> core::result::Result<Ed25519Offsets, Ed25519ParseError> {
    // Minimum size: 2 bytes header + 14 bytes per signature
    if ix_data.len() < ED25519_MIN_INSTRUCTION_SIZE {
        return Err(Ed25519ParseError::InstructionTooShort);
    }

    let num_signatures = ix_data[0];
    if num_signatures == 0 {
        return Err(Ed25519ParseError::NoSignatures);
    }

    // Parse offsets for first signature (starting at byte 2)
    let offsets_start = 2;
    let signature_offset =
        u16::from_le_bytes([ix_data[offsets_start], ix_data[offsets_start + 1]]) as usize;
    let signature_ix_index =
        u16::from_le_bytes([ix_data[offsets_start + 2], ix_data[offsets_start + 3]]);
    let pubkey_offset =
        u16::from_le_bytes([ix_data[offsets_start + 4], ix_data[offsets_start + 5]]) as usize;
    let pubkey_ix_index =
        u16::from_le_bytes([ix_data[offsets_start + 6], ix_data[offsets_start + 7]]);
    let message_offset =
        u16::from_le_bytes([ix_data[offsets_start + 8], ix_data[offsets_start + 9]]) as usize;
    let message_size =
        u16::from_le_bytes([ix_data[offsets_start + 10], ix_data[offsets_start + 11]]) as usize;
    let message_ix_index =
        u16::from_le_bytes([ix_data[offsets_start + 12], ix_data[offsets_start + 13]]);

    // Validate instruction indices: 0xFFFF means data is in the current instruction.
    // We require all data to be in the Ed25519 instruction itself, not elsewhere.
    if signature_ix_index != CURRENT_INSTRUCTION_INDEX
        || pubkey_ix_index != CURRENT_INSTRUCTION_INDEX
        || message_ix_index != CURRENT_INSTRUCTION_INDEX
    {
        return Err(Ed25519ParseError::ExternalDataReference);
    }

    Ok(Ed25519Offsets {
        num_signatures,
        signature_offset,
        signature_ix_index,
        pubkey_offset,
        pubkey_ix_index,
        message_offset,
        message_size,
        message_ix_index,
    })
}

/// Validate that pubkey and message in instruction data match expected values.
///
/// This is a pure function that can be unit tested without mocking sysvars.
///
/// # Arguments
/// * `ix_data` - Raw instruction data bytes
/// * `offsets` - Parsed offsets from parse_ed25519_offsets
/// * `expected_pubkey` - The Ed25519 public key we expect (32 bytes)
/// * `expected_message` - The message we expect to be signed (32 bytes SHA-256 hash)
///
/// # Returns
/// Ok(()) if validation passes, error otherwise.
pub fn validate_ed25519_data(
    ix_data: &[u8],
    offsets: &Ed25519Offsets,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8],
) -> core::result::Result<(), Ed25519ParseError> {
    // Validate signature bounds (64 bytes)
    if offsets.signature_offset + 64 > ix_data.len() {
        return Err(Ed25519ParseError::SignatureOutOfBounds);
    }

    // Validate pubkey bounds (32 bytes)
    if offsets.pubkey_offset + 32 > ix_data.len() {
        return Err(Ed25519ParseError::PubkeyOutOfBounds);
    }

    // Extract and verify public key
    let pubkey_in_ix: &[u8; 32] = ix_data[offsets.pubkey_offset..offsets.pubkey_offset + 32]
        .try_into()
        .map_err(|_| Ed25519ParseError::PubkeyOutOfBounds)?;

    if pubkey_in_ix != expected_pubkey {
        return Err(Ed25519ParseError::PubkeyMismatch);
    }

    // Validate message bounds
    if offsets.message_offset + offsets.message_size > ix_data.len() {
        return Err(Ed25519ParseError::MessageOutOfBounds);
    }

    // Message size must match the expected message
    if offsets.message_size != expected_message.len() {
        return Err(Ed25519ParseError::InvalidMessageSize);
    }

    // Extract and verify message
    let message_in_ix =
        &ix_data[offsets.message_offset..offsets.message_offset + offsets.message_size];

    if message_in_ix != expected_message {
        return Err(Ed25519ParseError::MessageMismatch);
    }

    Ok(())
}

/// Verify that an Ed25519 signature verification instruction exists at the specified index
/// and that it verifies the expected pubkey and message.
///
/// # Arguments
/// * `ix_sysvar` - The instructions sysvar account
/// * `ix_index` - Index of the Ed25519 instruction in the transaction
/// * `expected_pubkey` - The Ed25519 public key we expect (32 bytes)
/// * `expected_message` - The message we expect to be signed (120 bytes wrap message)
///
/// # Returns
/// Ok(()) if verification passes, error otherwise.
pub fn verify_ed25519_instruction(
    ix_sysvar: &AccountInfo,
    ix_index: u8,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8],
) -> Result<()> {
    // Validate the instructions sysvar
    require_keys_eq!(*ix_sysvar.key, IX_SYSVAR_ID, ErrorCode::InvalidInput);

    // Load the instruction at the specified index
    let ix = load_instruction_at_checked(ix_index as usize, ix_sysvar)
        .map_err(|_| ErrorCode::Ed25519InstructionNotFound)?;

    // Verify it's the Ed25519 program
    require_keys_eq!(
        ix.program_id,
        ed25519_program::ID,
        ErrorCode::InvalidEd25519Instruction
    );

    // Parse the Ed25519 instruction data
    let ix_data = &ix.data;
    let offsets = parse_ed25519_offsets(ix_data).map_err(|e| -> ErrorCode { e.into() })?;

    debug_msg!("Ed25519 instruction parsed:");
    debug_msg!("  signature_offset: {}", offsets.signature_offset);
    debug_msg!("  pubkey_offset: {}", offsets.pubkey_offset);
    debug_msg!(
        "  message_offset: {}, size: {}",
        offsets.message_offset,
        offsets.message_size
    );

    // Validate pubkey and message
    validate_ed25519_data(ix_data, &offsets, expected_pubkey, expected_message).map_err(|e| {
        match e {
            Ed25519ParseError::PubkeyMismatch => {
                msg!("Pubkey mismatch!");
                msg!("  expected: {:?}", &expected_pubkey[..8]);
            }
            Ed25519ParseError::MessageMismatch => {
                msg!("Message mismatch!");
                msg!("  expected: {:?}", &expected_message[..8]);
            }
            Ed25519ParseError::InvalidMessageSize => {
                msg!(
                    "Message size mismatch: expected 32, got {}",
                    offsets.message_size
                );
            }
            _ => {}
        }
        let code: ErrorCode = e.into();
        code
    })?;

    debug_msg!("  pubkey verified");
    debug_msg!("  message verified");

    // If we got here, the Ed25519 instruction exists and contains:
    // - The expected public key
    // - The expected message
    // - A signature (which the Ed25519 program will verify)
    //
    // If the Ed25519 instruction fails verification, the whole transaction fails.
    // If we reach this code, it means the transaction was accepted, so the signature is valid.

    debug_msg!("Ed25519 signature verification passed");
    Ok(())
}
