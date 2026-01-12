//! Journal digest computation for risc0-solana verification.

use crate::error::PAError;
use crate::types::ComplianceInstance;

/// Parse a ComplianceInstance from raw journal bytes.
/// This mirrors arm-risc0's journal_to_instance function.
pub fn parse_compliance_instance(instance_bytes: &[u8]) -> Result<ComplianceInstance, PAError> {
    bincode::deserialize(instance_bytes).map_err(|_| PAError::InvalidProof)
}
