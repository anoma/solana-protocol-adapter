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
const SIGNATURE_OFFSETS_SERIALIZED_SIZE: usize = 14; // 7 x u16

/// Verify that an Ed25519 signature verification instruction exists at the specified index
/// and that it verifies the expected pubkey and message.
///
/// # Arguments
/// * `ix_sysvar` - The instructions sysvar account
/// * `ix_index` - Index of the Ed25519 instruction in the transaction
/// * `expected_pubkey` - The Ed25519 public key we expect (32 bytes)
/// * `expected_message` - The message we expect to be signed (typically SHA-256 hash, 32 bytes)
///
/// # Returns
/// Ok(()) if verification passes, error otherwise.
pub fn verify_ed25519_instruction(
    ix_sysvar: &AccountInfo,
    ix_index: u8,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8; 32],
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

    // Minimum size: 2 bytes header + 14 bytes per signature
    if ix_data.len() < 2 + SIGNATURE_OFFSETS_SERIALIZED_SIZE {
        return Err(ErrorCode::InvalidEd25519Instruction.into());
    }

    let num_signatures = ix_data[0];
    if num_signatures == 0 {
        return Err(ErrorCode::InvalidEd25519Instruction.into());
    }

    // Parse offsets for first signature (starting at byte 2)
    // We verify only the first signature; if multiple are needed, this could be extended.
    let offsets_start = 2;
    let signature_offset = u16::from_le_bytes([
        ix_data[offsets_start],
        ix_data[offsets_start + 1],
    ]) as usize;
    let signature_ix_index = u16::from_le_bytes([
        ix_data[offsets_start + 2],
        ix_data[offsets_start + 3],
    ]);
    let pubkey_offset = u16::from_le_bytes([
        ix_data[offsets_start + 4],
        ix_data[offsets_start + 5],
    ]) as usize;
    let pubkey_ix_index = u16::from_le_bytes([
        ix_data[offsets_start + 6],
        ix_data[offsets_start + 7],
    ]);
    let message_offset = u16::from_le_bytes([
        ix_data[offsets_start + 8],
        ix_data[offsets_start + 9],
    ]) as usize;
    let message_size = u16::from_le_bytes([
        ix_data[offsets_start + 10],
        ix_data[offsets_start + 11],
    ]) as usize;
    let message_ix_index = u16::from_le_bytes([
        ix_data[offsets_start + 12],
        ix_data[offsets_start + 13],
    ]);

    // Validate instruction indices: 0xFFFF means data is in the current instruction.
    // We require all data to be in the Ed25519 instruction itself, not elsewhere.
    const CURRENT_INSTRUCTION: u16 = 0xFFFF;
    if signature_ix_index != CURRENT_INSTRUCTION
        || pubkey_ix_index != CURRENT_INSTRUCTION
        || message_ix_index != CURRENT_INSTRUCTION
    {
        msg!("Ed25519 data must be in the same instruction (expected 0xFFFF indices)");
        return Err(ErrorCode::InvalidEd25519Instruction.into());
    }

    msg!("Ed25519 instruction parsed:");
    msg!("  signature_offset: {}", signature_offset);
    msg!("  pubkey_offset: {}", pubkey_offset);
    msg!("  message_offset: {}, size: {}", message_offset, message_size);

    // Extract and verify public key
    if pubkey_offset + 32 > ix_data.len() {
        return Err(ErrorCode::InvalidEd25519Instruction.into());
    }
    let pubkey_in_ix: &[u8; 32] = ix_data[pubkey_offset..pubkey_offset + 32]
        .try_into()
        .map_err(|_| ErrorCode::InvalidEd25519Instruction)?;

    if pubkey_in_ix != expected_pubkey {
        msg!("Pubkey mismatch!");
        msg!("  expected: {:?}", &expected_pubkey[..8]);
        msg!("  got: {:?}", &pubkey_in_ix[..8]);
        return Err(ErrorCode::Ed25519PubkeyMismatch.into());
    }
    msg!("  pubkey verified");

    // Extract and verify message
    if message_offset + message_size > ix_data.len() {
        return Err(ErrorCode::InvalidEd25519Instruction.into());
    }

    // The expected message is 32 bytes (SHA-256 hash)
    if message_size != 32 {
        msg!("Message size mismatch: expected 32, got {}", message_size);
        return Err(ErrorCode::Ed25519MessageMismatch.into());
    }

    let message_in_ix: &[u8; 32] = ix_data[message_offset..message_offset + 32]
        .try_into()
        .map_err(|_| ErrorCode::InvalidEd25519Instruction)?;

    if message_in_ix != expected_message {
        msg!("Message mismatch!");
        msg!("  expected: {:?}", &expected_message[..8]);
        msg!("  got: {:?}", &message_in_ix[..8]);
        return Err(ErrorCode::Ed25519MessageMismatch.into());
    }
    msg!("  message verified");

    // If we got here, the Ed25519 instruction exists and contains:
    // - The expected public key
    // - The expected message
    // - A signature (which the Ed25519 program will verify)
    //
    // If the Ed25519 instruction fails verification, the whole transaction fails.
    // If we reach this code, it means the transaction was accepted, so the signature is valid.

    msg!("Ed25519 signature verification passed");
    Ok(())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    // Ed25519 instruction introspection is difficult to unit test
    // without mocking the sysvar. Integration tests will cover this.

    #[test]
    fn test_offset_size() {
        // Verify our constant matches the expected size
        assert_eq!(super::SIGNATURE_OFFSETS_SERIALIZED_SIZE, 14);
    }
}
