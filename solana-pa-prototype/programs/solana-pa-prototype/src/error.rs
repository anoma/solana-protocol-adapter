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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_calls::{decode_external_call, verify_output};
    use crate::groth16::prepare_proof_for_verification;
    use crate::test_utils::create_minimal_transaction;
    use crate::txdata::TxData;
    use crate::types::{ExpirableBlob, OutputMode};

    #[test]
    fn test_error_external_call_output_mismatch() {
        let expected = vec![1, 2, 3];
        let actual = vec![4, 5, 6];

        let result = verify_output(&expected, &actual, &OutputMode::ReturnData);
        assert!(result.is_err());

        match result {
            Err(PAError::ExternalCallOutputMismatch) => {}
            _ => panic!("Expected ExternalCallOutputMismatch error"),
        }
    }

    #[test]
    fn test_error_txdata_expired() {
        let txdata = TxData::new(100, 1000);
        let current_slot = 2000;

        let result = txdata.validate_not_expired(current_slot);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_invalid_external_call_blob() {
        let blob = ExpirableBlob {
            blob: vec![0xDEAD, 0xBEEF], // Garbage
            deletion_criterion: 0,
        };

        let result = decode_external_call(&blob);
        match result {
            Err(PAError::InvalidExternalCallBlob) => {}
            _ => panic!("Expected InvalidExternalCallBlob error"),
        }
    }

    #[test]
    fn test_error_invalid_aggregation_proof_bytes() {
        let mut tx = create_minimal_transaction();
        tx.aggregation_proof = Some(vec![0xFF; 10]); // Invalid

        let result = prepare_proof_for_verification(&tx);
        match result {
            Err(PAError::InvalidProof) | Err(PAError::UnsupportedProofType) => {}
            _ => panic!("Expected InvalidProof or UnsupportedProofType error"),
        }
    }
}
