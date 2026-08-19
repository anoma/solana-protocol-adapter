//! Groth16 proof extraction and preparation for risc0-solana verification.

use crate::error::PAError;
use arm_core::transaction::Aggregation;

use anchor_lang::prelude::AnchorDeserialize;
use verifier_router::Seal;

pub use arm_core::constants::BATCH_AGGREGATION_VK as BATCH_AGGREGATION_IMAGE_ID;

/// Prepared proof data for risc0-solana verification.
#[derive(Clone)]
pub struct PreparedProof {
    pub seal: Seal,
    pub image_id: [u8; 32],
    pub journal_digest: [u8; 32],
}

/// Prepare proof data for verification.
/// Extracts the seal, validates the selector, negates pi_a, computes the
/// journal digest from the aggregation instance, and selects the image ID.
pub fn prepare_proof_for_verification(
    aggregation: &Aggregation,
    expected_selector: [u8; 4],
) -> Result<PreparedProof, PAError> {
    let mut seal: Seal =
        Seal::try_from_slice(&aggregation.proof).map_err(|_| PAError::InvalidProof)?;

    // Validate proof selector matches what was configured at initialization.
    // EVM PA validates selector before verification; wrong-selector proofs would fail
    // at the verifier anyway, but this gives an explicit error message.
    if seal.selector != expected_selector {
        return Err(PAError::InvalidProofSelector);
    }

    // The risc0-solana groth16 verifier expects pi_a to be negated (on BN254 G1).
    seal.proof.pi_a = groth_16_verifier::negate_g1(&seal.proof.pi_a);

    // The journal bytes are re-derived from the typed instance, so every field
    // the settlement acts on is bound by the proof.
    let journal_digest = arm_solana::journal::aggregation_journal_digest(&aggregation.instance);
    Ok(PreparedProof {
        seal,
        image_id: BATCH_AGGREGATION_IMAGE_ID.into(),
        journal_digest,
    })
}
