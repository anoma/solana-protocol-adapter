//! Error types for the Protocol Adapter.

use anchor_lang::prelude::*;

/// Protocol Adapter errors.
#[error_code]
pub enum PAError {
    // =========================================================================
    // Nullifier errors
    // =========================================================================
    #[msg("Duplicate nullifier detected")]
    DuplicateNullifier,
    #[msg("Nullifier PDA pubkey mismatch")]
    NullifierPdaMismatch,

    // =========================================================================
    // Root marker errors
    // =========================================================================
    #[msg("Commitment tree root does not exist in historical set")]
    NonExistingRoot,
    #[msg("Root marker PDA pubkey mismatch")]
    RootPdaMismatch,
    #[msg("Root marker already exists")]
    RootAlreadyExists,

    // =========================================================================
    // TxData errors
    // =========================================================================
    #[msg("TxData has expired")]
    TxDataExpired,
    #[msg("TxData write exceeds payload capacity")]
    TxDataBoundsExceeded,
    #[msg("TxData expires_slot is below minimum (too soon)")]
    TxDataExpiryTooSoon,
    #[msg("TxData expires_slot exceeds maximum (too far in future)")]
    TxDataExpiryTooLate,
    #[msg("TxData extension must increase expires_slot")]
    TxDataExtendMustIncrease,
    #[msg("TxData has not expired yet (permissionless close requires expiration)")]
    TxDataNotExpired,
    #[msg("Invalid expiry configuration (min must be < max, within reasonable bounds)")]
    InvalidExpiryConfig,

    // =========================================================================
    // Data parsing errors
    // =========================================================================
    #[msg("Invalid transaction data")]
    InvalidTransactionData,

    // =========================================================================
    // Proof verification errors
    // =========================================================================
    #[msg("Invalid proof")]
    InvalidProof,
    #[msg("Unsupported proof type (expected Groth16 receipt)")]
    UnsupportedProofType,
    #[msg("Proof verification failed")]
    ProofVerificationFailed,
    #[msg("Verifier router call failed (verifier may be estopped)")]
    VerifierRouterFailed,
    #[msg("Aggregation required: non-aggregated proofs not enabled")]
    AggregationRequired,

    // =========================================================================
    // External call errors
    // =========================================================================
    #[msg("Invalid external call blob encoding")]
    InvalidExternalCallBlob,
    #[msg("Forwarder is not registered")]
    UnregisteredForwarder,
    #[msg("External call output verification failed")]
    ExternalCallOutputMismatch,
    #[msg("External call CPI failed")]
    ExternalCallCpiFailed,

    // =========================================================================
    // Delta proof errors
    // =========================================================================
    #[msg("Delta proof verification failed")]
    DeltaProofVerificationFailed,
    #[msg("Delta mismatch: transaction is not balanced")]
    DeltaMismatch,
    #[msg("Invalid delta proof format")]
    InvalidDeltaProof,
    #[msg("Delta point not on secp256k1 curve")]
    DeltaPointNotOnCurve,
    #[msg("Expected delta proof, got witness")]
    ExpectedDeltaProof,

    // =========================================================================
    // Logic verification errors
    // =========================================================================
    #[msg("Logic verifier input not found for tag")]
    TagNotFound,
    #[msg("Logic reference mismatch: verifying key does not match compliance instance")]
    LogicRefMismatch,
    #[msg("Tag count mismatch: logic_verifier_inputs.len() != 2 * compliance_units.len()")]
    TagCountMismatch,

    // =========================================================================
    // Protocol state errors
    // =========================================================================
    #[msg("Protocol adapter is paused")]
    Paused,
    #[msg("Unauthorized: caller is not the authority")]
    Unauthorized,
    #[msg("Already paused")]
    AlreadyPaused,

    // =========================================================================
    // Merkle tree errors
    // =========================================================================
    #[msg("Tree has reached maximum depth (32 levels)")]
    TreeMaxDepthReached,
}

impl From<anchor_lang::solana_program::program_error::ProgramError> for PAError {
    fn from(_: anchor_lang::solana_program::program_error::ProgramError) -> Self {
        PAError::ExternalCallCpiFailed
    }
}

