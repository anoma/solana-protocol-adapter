//! Ed25519 signature verification via instruction introspection.
//!
//! Solana's Ed25519 program is a precompile that cannot be called by CPI.
//! Instead the transaction carries an Ed25519 verify instruction, and this
//! program checks through the instructions sysvar that the instruction at
//! the index the wrap input names verifies the expected public key over
//! the expected message. The precompile has already verified the signature
//! by the time this program runs, or the transaction would have failed.
//!
//! Reference: https://rareskills.io/post/solana-signature-verification

use anchor_lang::prelude::*;
use solana_instructions_sysvar::{load_instruction_at_checked, ID as IX_SYSVAR_ID};
use solana_sdk_ids::ed25519_program;

use crate::ErrorCode;

/// Ed25519 instruction data: `num_signatures: u8`, `padding: u8`, then per
/// signature seven u16 LE fields (signature offset and instruction index,
/// public key offset and instruction index, message offset, size and
/// instruction index), then the data those offsets point at.
pub const ED25519_MIN_INSTRUCTION_SIZE: usize = 16;
/// Instruction index meaning "the data is in this instruction".
pub const CURRENT_INSTRUCTION_INDEX: u16 = 0xFFFF;

/// Where the first signature's public key and message lie within the
/// instruction data. The signature itself is never read: the precompile
/// verified it before this program ran.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ed25519Offsets {
    pub pubkey_offset: usize,
    pub message_offset: usize,
    pub message_size: usize,
}

/// Parse the first signature's offsets. Every instruction index must be
/// `CURRENT_INSTRUCTION_INDEX`: data referenced from another instruction is
/// not verified against this instruction's bytes.
pub fn parse_ed25519_offsets(ix_data: &[u8]) -> core::result::Result<Ed25519Offsets, ErrorCode> {
    if ix_data.len() < ED25519_MIN_INSTRUCTION_SIZE || ix_data[0] == 0 {
        return Err(ErrorCode::InvalidEd25519Instruction);
    }
    let field = |i: usize| u16::from_le_bytes([ix_data[2 + 2 * i], ix_data[3 + 2 * i]]);

    let signature_ix_index = field(1);
    let pubkey_ix_index = field(3);
    let message_ix_index = field(6);
    if signature_ix_index != CURRENT_INSTRUCTION_INDEX
        || pubkey_ix_index != CURRENT_INSTRUCTION_INDEX
        || message_ix_index != CURRENT_INSTRUCTION_INDEX
    {
        return Err(ErrorCode::InvalidEd25519Instruction);
    }

    Ok(Ed25519Offsets {
        pubkey_offset: field(2) as usize,
        message_offset: field(4) as usize,
        message_size: field(5) as usize,
    })
}

/// Check that the instruction's public key and message are the expected ones.
pub fn validate_ed25519_data(
    ix_data: &[u8],
    offsets: &Ed25519Offsets,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8],
) -> core::result::Result<(), ErrorCode> {
    let pubkey_in_ix = ix_data
        .get(offsets.pubkey_offset..offsets.pubkey_offset + 32)
        .ok_or(ErrorCode::InvalidEd25519Instruction)?;
    if pubkey_in_ix != expected_pubkey {
        return Err(ErrorCode::Ed25519PubkeyMismatch);
    }

    let message_in_ix = ix_data
        .get(offsets.message_offset..offsets.message_offset + offsets.message_size)
        .ok_or(ErrorCode::InvalidEd25519Instruction)?;
    if message_in_ix != expected_message {
        return Err(ErrorCode::Ed25519MessageMismatch);
    }
    Ok(())
}

/// Verify that the Ed25519 instruction at `ix_index` verifies
/// `expected_pubkey` over `expected_message`.
pub fn verify_ed25519_instruction(
    ix_sysvar: &AccountInfo,
    ix_index: u8,
    expected_pubkey: &[u8; 32],
    expected_message: &[u8],
) -> Result<()> {
    require_keys_eq!(*ix_sysvar.key, IX_SYSVAR_ID, ErrorCode::InvalidInput);

    let ix = load_instruction_at_checked(ix_index as usize, ix_sysvar)
        .map_err(|_| ErrorCode::Ed25519InstructionNotFound)?;
    require_keys_eq!(
        ix.program_id,
        ed25519_program::ID,
        ErrorCode::InvalidEd25519Instruction
    );

    let offsets = parse_ed25519_offsets(&ix.data)?;
    validate_ed25519_data(&ix.data, &offsets, expected_pubkey, expected_message)?;
    Ok(())
}
