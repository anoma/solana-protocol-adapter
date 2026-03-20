//! Groth16 proof extraction and preparation for risc0-solana verification.

use crate::error::PAError;
use arm_core::transaction::Transaction;

use anchor_lang::prelude::AnchorDeserialize;
use verifier_router::Seal;

pub use arm_core::constants::BATCH_AGGREGATION_VK_BYTES as BATCH_AGGREGATION_IMAGE_ID;

/// Prepared proof data for risc0-solana verification.
#[derive(Clone)]
pub struct PreparedProof {
    pub seal: Seal,
    pub image_id: [u8; 32],
    pub journal_digest: [u8; 32],
}

/// Prepare proof data for verification.
/// Extracts seal, validates selector, negates pi_a, computes journal digest, and selects image ID.
pub fn prepare_proof_for_verification(
    tx: &Transaction,
    expected_selector: [u8; 4],
) -> Result<PreparedProof, PAError> {
    let proof_bytes = tx.aggregation_proof.as_ref().ok_or(PAError::InvalidProof)?;

    let mut seal: Seal = Seal::try_from_slice(proof_bytes).map_err(|_| PAError::InvalidProof)?;

    // Validate proof selector matches what was configured at initialization.
    // EVM PA validates selector before verification; wrong-selector proofs would fail
    // at the verifier anyway, but this gives an explicit error message.
    if seal.selector != expected_selector {
        return Err(PAError::InvalidProofSelector);
    }

    // The risc0-solana groth16 verifier expects pi_a to be negated (on BN254 G1).
    seal.proof.pi_a = groth_16_verifier::negate_g1(&seal.proof.pi_a);

    let journal_digest = crate::encoding::compute_batch_aggregation_journal_digest(tx)?.to_bytes();
    Ok(PreparedProof {
        seal,
        image_id: BATCH_AGGREGATION_IMAGE_ID,
        journal_digest,
    })
}
