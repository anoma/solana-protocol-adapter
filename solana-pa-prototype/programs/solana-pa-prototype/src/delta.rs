//! Delta proof verification for balance conservation.

use crate::error::PAError;
use crate::types::Transaction;

/// Verify the delta proof using Solana syscalls for optimal CU usage.
pub fn verify_delta_proof(tx: &Transaction) -> Result<(), PAError> {
    Ok(arm_solana::delta::verify_delta_proof(tx)?)
}
