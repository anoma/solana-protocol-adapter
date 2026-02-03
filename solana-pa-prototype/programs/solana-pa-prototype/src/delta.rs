//! Delta proof verification for balance conservation.
//!
//! Delegates to arm-risc0's `solana_delta` module which uses Solana syscalls
//! (hashv, secp256k1_recover) and solana-secp256k1 for EC point arithmetic.
//! This avoids k256 which exceeds SBF stack frame limits.

use crate::error::PAError;
use crate::types::Transaction;

use anoma_rm_risc0::error::ArmError;

// Re-export for tests
pub use anoma_rm_risc0::solana_delta::{accumulate_deltas, collect_tags, compute_verifying_key};

/// Map arm-risc0 delta errors to PA error codes.
fn map_arm_error(e: ArmError) -> PAError {
    match e {
        ArmError::ExpectedDeltaProof => PAError::ExpectedDeltaProof,
        ArmError::InvalidDeltaProof => PAError::InvalidDeltaProof,
        ArmError::DeltaPointNotOnCurve => PAError::DeltaPointNotOnCurve,
        ArmError::DeltaProofVerificationFailed => PAError::DeltaProofVerificationFailed,
        ArmError::DeltaMismatch => PAError::DeltaMismatch,
        _ => PAError::DeltaProofVerificationFailed,
    }
}

/// Verify the delta proof using Solana syscalls for optimal CU usage.
pub fn verify_delta_proof(tx: &Transaction) -> Result<(), PAError> {
    anoma_rm_risc0::solana_delta::verify_delta_proof(tx).map_err(map_arm_error)
}
