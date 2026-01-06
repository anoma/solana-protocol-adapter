//! Custom error codes for the SPL Token Forwarder.

use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Invalid input data")]
    InvalidInput,

    #[msg("Unknown operation code")]
    UnknownOperation,

    #[msg("Unauthorized caller - only Protocol Adapter can call forward_call")]
    UnauthorizedCaller,

    #[msg("Unauthorized logic_ref - this forwarder doesn't handle this resource type")]
    UnauthorizedLogicRef,

    #[msg("Forwarder is in emergency stopped state")]
    EmergencyStopped,

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

    #[msg("Zero address not allowed")]
    ZeroAddressNotAllowed,

    #[msg("Account not found in remaining accounts")]
    AccountNotFound,

    #[msg("Invalid account owner")]
    InvalidAccountOwner,

    #[msg("Invalid escrow PDA")]
    InvalidEscrowPda,

    #[msg("Invalid nonce PDA")]
    InvalidNoncePda,

    #[msg("Token transfer failed")]
    TokenTransferFailed,
}
