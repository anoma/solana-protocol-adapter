//! Delta proof verification for balance conservation.

use crate::error::PAError;
use crate::types::Transaction;

use arm_solana::SolanaArmError;

/// Map arm-solana delta errors to PA error codes.
fn map_arm_error(e: SolanaArmError) -> PAError {
    match e {
        SolanaArmError::ExpectedDeltaProof => PAError::ExpectedDeltaProof,
        SolanaArmError::InvalidDeltaProof => PAError::InvalidDeltaProof,
        SolanaArmError::DeltaPointNotOnCurve => PAError::DeltaPointNotOnCurve,
        SolanaArmError::DeltaProofVerificationFailed => PAError::DeltaProofVerificationFailed,
        SolanaArmError::DeltaMismatch => PAError::DeltaMismatch,
    }
}

/// Verify the delta proof using Solana syscalls for optimal CU usage.
pub fn verify_delta_proof(tx: &Transaction) -> Result<(), PAError> {
    arm_solana::delta::verify_delta_proof(tx).map_err(map_arm_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_arm_error_all_variants() {
        assert!(matches!(
            map_arm_error(SolanaArmError::ExpectedDeltaProof),
            PAError::ExpectedDeltaProof
        ));
        assert!(matches!(
            map_arm_error(SolanaArmError::InvalidDeltaProof),
            PAError::InvalidDeltaProof
        ));
        assert!(matches!(
            map_arm_error(SolanaArmError::DeltaPointNotOnCurve),
            PAError::DeltaPointNotOnCurve
        ));
        assert!(matches!(
            map_arm_error(SolanaArmError::DeltaProofVerificationFailed),
            PAError::DeltaProofVerificationFailed
        ));
        assert!(matches!(
            map_arm_error(SolanaArmError::DeltaMismatch),
            PAError::DeltaMismatch
        ));
    }
}
