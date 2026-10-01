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
    #[msg("Root marker is missing for, does not match, or is passed without a root this settlement produces")]
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
    #[msg("Forwarder account does not match the program the external call names")]
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

    // Aggregation instance binding errors
    #[msg("Aggregation instance compliance key does not match the compliance circuit VK")]
    ComplianceKeyMismatch,
    #[msg("Aggregation instance kind-table commitment does not match the configured table")]
    KindTableCommitmentMismatch,
    #[msg("Zero kind-table commitment not allowed")]
    ZeroKindTableCommitment,
    #[msg("Duplicate nullifier within the aggregation instance")]
    NullifierDuplication,

    // Protocol state errors
    #[msg("Protocol adapter is stopped")]
    Stopped,
    #[msg("Unauthorized: the signer does not hold the authority this instruction requires")]
    Unauthorized,
    #[msg("Protocol adapter is already stopped")]
    AlreadyStopped,
    #[msg("Operation requires the PA to be stopped")]
    NotStopped,

    // Merkle tree errors
    #[msg("Tree has reached maximum depth (32 levels)")]
    TreeMaxDepthReached,

    // Close errors
    #[msg("Account is not owned by this program")]
    InvalidMarker,

    // External call encoding
    #[msg("External call expected_output must be non-empty: Solana cannot represent an explicit empty return")]
    EmptyExpectedOutput,

    // Marker creation
    #[msg("Marker address is held by an unexpected owner")]
    MarkerUnexpectedOwner,
    #[msg("Marker address already contains data")]
    MarkerUnexpectedData,

    // Root retention
    #[msg("Root marker already exists: the commitment tree produced a repeated root")]
    RootMarkerAlreadyExists,

    // State layout
    #[msg("PAState schema version is not the one this program binary reads; migrate the account first")]
    UnsupportedStateSchema,
    #[msg("PAState is not a state account in the previous schema version")]
    NotPreviousSchema,

    // Logic-ref denylist
    #[msg("Zero logic ref not allowed")]
    ZeroLogicRefNotAllowed,
    #[msg("Logic ref is already denied")]
    LogicRefAlreadyDenied,
    #[msg("A resource carries a denied logic ref")]
    DeniedLogicRef,

    // Initialization, as pa-evm's zero-value rejections
    #[msg("Zero verifier router not allowed")]
    ZeroVerifierRouterNotAllowed,
    #[msg("Zero proof selector not allowed")]
    ZeroProofSelectorNotAllowed,
}

impl From<SolanaArmError> for PAError {
    fn from(e: SolanaArmError) -> Self {
        match e {
            SolanaArmError::MissingAggregation => PAError::AggregationRequired,
            SolanaArmError::AmbiguousTransaction => PAError::InvalidTransactionData,
            SolanaArmError::ExpectedDeltaProof => PAError::ExpectedDeltaProof,
            SolanaArmError::InvalidDeltaProof => PAError::InvalidDeltaProof,
            SolanaArmError::DeltaPointNotOnCurve => PAError::DeltaPointNotOnCurve,
            SolanaArmError::DeltaProofVerificationFailed => PAError::DeltaProofVerificationFailed,
            SolanaArmError::DeltaMismatch => PAError::DeltaMismatch,
        }
    }
}
