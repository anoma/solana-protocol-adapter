//! Groth16 proof extraction and preparation for risc0-solana verification.
//!
//! This module defines the types locally to avoid depending on the risc0-solana submodule.
//! The type definitions match exactly what the deployed risc0 verifier programs expect.

use crate::error::PAError;
use crate::types::Transaction;
use anchor_lang::prelude::*;

// ============================================================================
// Type definitions (matching risc0-solana exactly for CPI compatibility)
// ============================================================================

/// Groth16 proof elements on BN254 curve.
/// Matches `groth_16_verifier::Proof` exactly.
#[derive(Clone, PartialEq, Eq, AnchorDeserialize, AnchorSerialize)]
pub struct Proof {
    /// G1 point (must be negated before verification)
    pub pi_a: [u8; 64],
    /// G2 point
    pub pi_b: [u8; 128],
    /// G1 point
    pub pi_c: [u8; 64],
}

/// Verifier selector - identifies which verifier version to use.
pub type Selector = [u8; 4];

/// An encoded RISC Zero proof along with a selector.
/// Matches `verifier_router::Seal` exactly.
#[derive(Clone, PartialEq, Eq, AnchorDeserialize, AnchorSerialize)]
pub struct Seal {
    pub selector: Selector,
    pub proof: Proof,
}

// ============================================================================
// Official risc0 devnet program IDs
// ============================================================================

/// Official risc0 groth16_verifier program ID (devnet).
pub const GROTH16_VERIFIER_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk");

/// Official risc0 verifier_router program ID (devnet).
pub const VERIFIER_ROUTER_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq");

/// Image ID for batch aggregation circuit (from arm-risc0 constants.rs).
pub const BATCH_AGGREGATION_IMAGE_ID: [u8; 32] =
    hex_literal::hex!("4297db7ea74c296acd49ca55e160a6e930478da2605931ea0654d466c0f95099");

/// Prepared proof data for risc0-solana verification.
#[derive(Clone)]
pub struct PreparedProof {
    pub seal: Seal,
    pub image_id: [u8; 32],
    pub journal_digest: [u8; 32],
}

/// Prepare proof data for verification.
/// The aggregation_proof is expected to be a prepared Seal (with pi_a already negated).
pub fn prepare_proof_for_verification(
    tx: &Transaction,
) -> core::result::Result<PreparedProof, PAError> {
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
