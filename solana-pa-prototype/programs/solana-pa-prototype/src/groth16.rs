//! Groth16 proof extraction and preparation for RISC Zero verifier router
//! verification.

use crate::error::PAError;
use crate::verifier_router::types::Seal;
use arm_core::constants::BATCH_AGGREGATION_VK;
use arm_core::transaction::Aggregation;

use anchor_lang::prelude::AnchorDeserialize;
use ark_bn254::{Fq, G1Affine};
use ark_ff::{BigInt, BigInteger, PrimeField};

/// Prepared proof data for verifier router verification.
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
        return Err(PAError::RiscZeroVerifierSelectorMismatch);
    }

    // The RISC Zero Groth16 verifier expects pi_a to be negated (on BN254 G1).
    seal.proof.pi_a = negate_g1(&seal.proof.pi_a)?;

    // The journal bytes are re-derived from the typed instance, so every field
    // the settlement acts on is bound by the proof.
    let journal_digest = arm_solana::journal::aggregation_journal_digest(&aggregation.instance);
    Ok(PreparedProof {
        seal,
        image_id: BATCH_AGGREGATION_VK.into(),
        journal_digest,
    })
}

/// Negate a BN254 G1 point encoded as big-endian `x ‖ y` (the alt_bn128
/// syscall encoding; all zeros is the point at infinity).
///
/// Coordinates must be canonical base-field elements. Curve and subgroup
/// membership are not checked here: the verifier's pairing syscall checks
/// them.
pub(crate) fn negate_g1(point: &[u8; 64]) -> Result<[u8; 64], PAError> {
    let (x, y) = point.split_at(32);
    let x = fq_from_be_bytes(x).ok_or(PAError::InvalidProof)?;
    let y = fq_from_be_bytes(y).ok_or(PAError::InvalidProof)?;
    let negated = -G1Affine::new_unchecked(x, y);

    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&negated.x.into_bigint().to_bytes_be());
    out[32..].copy_from_slice(&negated.y.into_bigint().to_bytes_be());
    Ok(out)
}

/// A 32-byte big-endian base-field element; `None` unless it is below the
/// field modulus.
fn fq_from_be_bytes(bytes: &[u8]) -> Option<Fq> {
    let mut limbs = [0u64; 4];
    for (limb, chunk) in limbs.iter_mut().zip(bytes.rchunks_exact(8)) {
        *limb = u64::from_be_bytes(chunk.try_into().ok()?);
    }
    Fq::from_bigint(BigInt::new(limbs))
}
