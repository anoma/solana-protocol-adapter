//! Error types for the Protocol Adapter.

use anchor_lang::prelude::*;
use arm_solana::SolanaArmError;

/// Protocol Adapter errors.
#[error_code]
pub enum PAError {
    // Nullifier errors
    #[msg("Duplicate nullifier detected")]
    DuplicateNullifier,
    #[msg("Nullifier PDA pubkey mismatch")]
    NullifierPdaMismatch,

    // Root marker errors
    #[msg("Commitment tree root does not exist in historical set")]
    NonExistingRoot,
    #[msg("Root marker PDA pubkey mismatch")]
    RootPdaMismatch,

    // TxData errors
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

    // Data parsing errors
    #[msg("Invalid transaction data")]
    InvalidTransactionData,

    // Proof verification errors
    #[msg("Invalid proof")]
    InvalidProof,
    #[msg("Verifier router call failed (verifier may be estopped)")]
    VerifierRouterFailed,
    #[msg("Aggregation required: non-aggregated proofs not enabled")]
    AggregationRequired,
    #[msg("Proof selector does not match expected selector")]
    InvalidProofSelector,

    // External call errors
    #[msg("Invalid external call blob encoding")]
    InvalidExternalCallBlob,
    #[msg("Forwarder is not registered")]
    UnregisteredForwarder,
    #[msg("External call output verification failed")]
    ExternalCallOutputMismatch,
    #[msg("External call CPI failed")]
    ExternalCallCpiFailed,

    // Delta proof errors
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

    // Logic verification errors
    #[msg("Logic verifier input not found for tag")]
    TagNotFound,

    // Protocol state errors
    #[msg("Protocol adapter is stopped")]
    Stopped,
    #[msg("Unauthorized: caller is not the authority")]
    Unauthorized,
    #[msg("Protocol adapter is already stopped")]
    AlreadyStopped,
    #[msg("No pending authority transfer to accept")]
    NoPendingAuthority,
    #[msg("Operation requires the PA to be stopped")]
    NotStopped,

    // Merkle tree errors
    #[msg("Tree has reached maximum depth (32 levels)")]
    TreeMaxDepthReached,

    // Close errors
    #[msg("Account is not owned by this program")]
    InvalidMarker,

    // Compliance instance parsing
    #[msg("Failed to parse compliance instance from journal bytes")]
    ComplianceInstanceParseFailed,
}

impl From<SolanaArmError> for PAError {
    fn from(e: SolanaArmError) -> Self {
        match e {
            SolanaArmError::ExpectedDeltaProof => PAError::ExpectedDeltaProof,
            SolanaArmError::InvalidDeltaProof => PAError::InvalidDeltaProof,
            SolanaArmError::DeltaPointNotOnCurve => PAError::DeltaPointNotOnCurve,
            SolanaArmError::DeltaProofVerificationFailed => PAError::DeltaProofVerificationFailed,
            SolanaArmError::DeltaMismatch => PAError::DeltaMismatch,
            SolanaArmError::ComplianceInstanceParseFailed => PAError::ComplianceInstanceParseFailed,
        }
    }
}
