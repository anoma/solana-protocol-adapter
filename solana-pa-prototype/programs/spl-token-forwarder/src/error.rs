//! Custom error codes for the SPL Token Forwarder.
//!
//! Error codes are designed to provide specific, actionable information.
//! Use `msg!()` logging before returning errors to provide runtime context
//! (e.g., expected vs actual values) since Anchor doesn't support error parameters.

use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    // =========================================================================
    // Input Validation Errors (mirrors EVM's revert with parameters via msg!())
    // =========================================================================
    /// Generic input validation failure. Check logs for expected/actual values.
    /// Mirrors EVM: `InvalidInputLength({expected: X, actual: Y})`
    #[msg("Invalid input data - check logs for expected vs actual length")]
    InvalidInput,

    /// Wrap operation input has wrong length. Expected 185 bytes.
    #[msg("Invalid wrap input length - expected 185 bytes")]
    InvalidWrapInputLength,

    /// Unwrap operation input has wrong length. Expected 72 bytes.
    #[msg("Invalid unwrap input length - expected 72 bytes")]
    InvalidUnwrapInputLength,

    /// Emergency withdraw input has wrong length. Expected 72 bytes.
    #[msg("Invalid emergency withdraw input length - expected 72 bytes")]
    InvalidEmergencyInputLength,

    /// Token account data is malformed or too short.
    #[msg("Invalid token account data - too short or malformed")]
    InvalidTokenAccountData,

    /// Not enough remaining accounts provided.
    /// Mirrors EVM: `InsufficientAccounts({expected: X, actual: Y})`
    #[msg("Insufficient remaining accounts - check logs for expected count")]
    InsufficientRemainingAccounts,

    /// Wrong token program ID provided.
    #[msg("Invalid token program - expected SPL Token program ID")]
    InvalidTokenProgram,

    /// Token mint account doesn't match expected mint.
    #[msg("Token mint mismatch - provided mint doesn't match input")]
    TokenMintMismatch,

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

    #[msg("Ed25519 signature verification failed")]
    InvalidSignature,

    #[msg("Ed25519 instruction not found at specified index")]
    Ed25519InstructionNotFound,

    #[msg("Invalid Ed25519 instruction format")]
    InvalidEd25519Instruction,

    #[msg("Ed25519 public key mismatch")]
    Ed25519PubkeyMismatch,

    #[msg("Ed25519 message mismatch")]
    Ed25519MessageMismatch,

    #[msg("Insufficient escrow balance for unwrap")]
    InsufficientEscrowBalance,

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

    #[msg("Account not found in remaining accounts")]
    AccountNotFound,

    #[msg("Invalid account owner")]
    InvalidAccountOwner,

    #[msg("Invalid escrow PDA")]
    InvalidEscrowPda,

    #[msg("Invalid nonce bitmap PDA - doesn't match derived address")]
    InvalidNonceBitmapPda,

    #[msg(
        "Nonce bitmap account does not exist - create it with init_nonce_bitmap before the wrap"
    )]
    NonceBitmapMissing,

    #[msg("Token transfer failed - ensure user has approved escrow PDA as delegate with sufficient amount")]
    TokenTransferFailed,

    #[msg("Insufficient delegate approval - user must approve escrow PDA as delegate before wrap")]
    InsufficientDelegateApproval,

    #[msg("Balance mismatch - actual transfer amount differs from expected (possible fee-on-transfer token)")]
    BalanceMismatch,
}
