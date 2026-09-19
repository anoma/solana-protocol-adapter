//! Error codes. Where a check has expected-versus-actual context, the
//! program logs it with `msg!` before returning, since Anchor errors carry
//! no parameters.

use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid input data")]
    InvalidInput,

    #[msg("Invalid wrap input length")]
    InvalidWrapInputLength,

    #[msg("Invalid unwrap input length - expected 72 bytes")]
    InvalidUnwrapInputLength,

    #[msg("Invalid token account data - too short or malformed")]
    InvalidTokenAccountData,

    #[msg("Insufficient remaining accounts - check logs for expected count")]
    InsufficientRemainingAccounts,

    #[msg("Invalid token program - expected SPL Token program ID")]
    InvalidTokenProgram,

    #[msg("Token account owner mismatch - the destination is not the escrow's or the recipient's")]
    WrongTokenAccountOwner,

    #[msg("Unknown operation code")]
    UnknownOperation,

    #[msg("Unauthorized caller - only Protocol Adapter can call forward_call")]
    UnauthorizedCaller,

    #[msg("Unauthorized logic_ref - this forwarder doesn't handle this resource type")]
    UnauthorizedLogicRef,

    #[msg("Signature deadline has expired")]
    DeadlineExpired,

    #[msg("Nonce has already been used (replay attack prevented)")]
    NonceAlreadyUsed,

    #[msg("Ed25519 instruction not found at specified index")]
    Ed25519InstructionNotFound,

    #[msg("Invalid Ed25519 instruction format")]
    InvalidEd25519Instruction,

    #[msg("Ed25519 public key mismatch")]
    Ed25519PubkeyMismatch,

    #[msg("Ed25519 message mismatch")]
    Ed25519MessageMismatch,

    #[msg("Emergency caller already set")]
    EmergencyCallerAlreadySet,

    #[msg("Emergency caller not set")]
    EmergencyCallerNotSet,

    #[msg("Protocol Adapter not stopped - cannot perform emergency operations")]
    ProtocolAdapterNotStopped,

    #[msg("Invalid PA state account - does not match derived PDA from protocol_adapter")]
    InvalidPaState,

    #[msg("Zero address not allowed")]
    ZeroAddressNotAllowed,

    #[msg("Invalid escrow PDA")]
    InvalidEscrowPda,

    #[msg("Invalid nonce bitmap PDA - doesn't match derived address")]
    InvalidNonceBitmapPda,

    #[msg(
        "Nonce bitmap account does not exist - create it with init_nonce_bitmap before the wrap"
    )]
    NonceBitmapMissing,

    #[msg("Token transfer failed - check the SPL Token error in the logs")]
    TokenTransferFailed,
}
