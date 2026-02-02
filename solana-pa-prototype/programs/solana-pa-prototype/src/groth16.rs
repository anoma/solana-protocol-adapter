//! Groth16 proof extraction and preparation for risc0-solana verification.

use crate::error::PAError;
use crate::types::Transaction;

use anchor_lang::prelude::AnchorDeserialize;
// Re-export types for use in lib.rs
// Proof comes from groth_16_verifier (verifier_router uses it but doesn't re-export)
pub use groth_16_verifier::Proof;
pub use verifier_router::{Seal, Selector};

// Import from arm-risc0
use anoma_rm_risc0::solana_constants::BATCH_AGGREGATION_VK_BYTES as BATCH_AGGREGATION_IMAGE_ID;

/// Prepared proof data for risc0-solana verification.
#[derive(Clone)]
pub struct PreparedProof {
    pub seal: Seal,
    pub image_id: [u8; 32],
    pub journal_digest: [u8; 32],
}

/// Prepare proof data for verification.
/// Extracts seal and selector, negates pi_a, computes journal digest, and selects image ID.
pub fn prepare_proof_for_verification(tx: &Transaction) -> Result<PreparedProof, PAError> {
    let proof_bytes: &Vec<u8> = tx.aggregation_proof.as_ref().ok_or(PAError::InvalidProof)?;

    let seal: Seal = Seal::try_from_slice(proof_bytes).map_err(|_| PAError::InvalidProof)?;
    let image_id = BATCH_AGGREGATION_IMAGE_ID;
    let journal_digest = crate::encoding::compute_batch_aggregation_journal_digest(tx)?.to_bytes();
    Ok(PreparedProof {
        seal,
        image_id,
        journal_digest,
    })
}
