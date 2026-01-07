//! Groth16 proof extraction and preparation for risc0-solana verification.

use crate::error::PAError;
use crate::types::Transaction;

// Re-export types for use in lib.rs
// Proof comes from groth_16_verifier (verifier_router uses it but doesn't re-export)
pub use groth_16_verifier::Proof;
pub use verifier_router::{Seal, Selector};

/// Image ID for batch aggregation circuit (from arm-risc0 constants.rs).
pub const BATCH_AGGREGATION_IMAGE_ID: [u8; 32] =
    hex_literal::hex!("5eeeb4e5b4db4548d6c0e21c35b54041cdceda63700b060470826ee2c92740a1");

/// BN254 base field modulus (big-endian bytes).
const BN254_MODULUS: [u8; 32] =
    hex_literal::hex!("30644e72e131a029b85045b68181585d97816a916871ca8d3c208c16d87cfd47");

/// Subtract two 256-bit big-endian numbers: result = a - b (assuming a >= b).
fn bigint_sub(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut result = [0u8; 32];
    let mut borrow: i16 = 0;

    for i in (0..32).rev() {
        let diff = (a[i] as i16) - (b[i] as i16) - borrow;
        if diff < 0 {
            result[i] = (diff + 256) as u8;
            borrow = 1;
        } else {
            result[i] = diff as u8;
            borrow = 0;
        }
    }
    result
}

/// Check if a 256-bit big-endian number is zero.
fn is_zero(bytes: &[u8; 32]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

/// Negate the y-coordinate of pi_a for Groth16 verification.
/// Required for on-chain verification with risc0-solana.
/// Inputs/outputs are big-endian as required by groth16-solana.
/// pi_a layout: x (32 bytes) + y (32 bytes), y is negated as p - y.
pub fn negate_pi_a(seal: &[u8; 256]) -> [u8; 256] {
    let mut result = *seal;

    let mut y = [0u8; 32];
    y.copy_from_slice(&seal[32..64]);

    if !is_zero(&y) {
        let neg_y = bigint_sub(&BN254_MODULUS, &y);
        result[32..64].copy_from_slice(&neg_y);
    }

    result
}

/// Extract the 256-byte seal from a proof.
/// The proof bytes should contain at least 256 bytes of Groth16 proof data.
pub fn extract_seal(proof_bytes: &[u8]) -> Result<[u8; 256], PAError> {
    if proof_bytes.len() < 256 {
        return Err(PAError::InvalidProof);
    }
    let mut seal = [0u8; 256];
    seal.copy_from_slice(&proof_bytes[..256]);
    Ok(seal)
}

fn read_u32_le(input: &[u8], offset: &mut usize) -> Result<u32, PAError> {
    if *offset + 4 > input.len() {
        return Err(PAError::InvalidProof);
    }
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&input[*offset..*offset + 4]);
    *offset += 4;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64_le(input: &[u8], offset: &mut usize) -> Result<u64, PAError> {
    if *offset + 8 > input.len() {
        return Err(PAError::InvalidProof);
    }
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&input[*offset..*offset + 8]);
    *offset += 8;
    Ok(u64::from_le_bytes(bytes))
}

fn read_bytes<'a>(input: &'a [u8], offset: &mut usize, len: usize) -> Result<&'a [u8], PAError> {
    if *offset + len > input.len() {
        return Err(PAError::InvalidProof);
    }
    let out = &input[*offset..*offset + len];
    *offset += len;
    Ok(out)
}

/// Extract selector from verifier_parameters digest at tail of proof bytes.
///
/// The verifier_parameters is a 32-byte Digest at the end of the serialized receipt.
/// The selector is its first 4 bytes.
fn extract_selector_from_tail(proof_bytes: &[u8]) -> Result<Selector, PAError> {
    if proof_bytes.len() < 32 {
        return Err(PAError::InvalidProof);
    }
    let vp_start = proof_bytes.len() - 32;
    let mut selector: Selector = [0u8; 4];
    selector.copy_from_slice(&proof_bytes[vp_start..vp_start + 4]);
    Ok(selector)
}

/// Extract the Groth16 seal bytes and selector from a bincode-serialized `InnerReceipt`
/// embedded inside an `AggregationProof`.
///
/// This is a minimal parser that supports only `InnerReceipt::Groth16`, which is the only receipt
/// type we support for on-chain verification.
///
/// Returns (selector, seal) where:
/// - selector: first 4 bytes of verifier_parameters (last 32 bytes of proof)
/// - seal: the 256-byte Groth16 proof
fn extract_groth16_seal_from_aggregation_proof(
    proof_bytes: &[u8],
) -> Result<(Selector, Vec<u8>), PAError> {
    let selector = extract_selector_from_tail(proof_bytes)?;

    // AggregationProof is a serde enum, bincode encodes a u32 discriminant.
    // Both variants wrap an InnerReceipt, so after the first 4 bytes the InnerReceipt begins.
    let mut offset = 0usize;
    let _strategy_variant = read_u32_le(proof_bytes, &mut offset)?;

    // InnerReceipt discriminant (Composite=0, Succinct=1, Groth16=2, Fake=3).
    let inner_variant = read_u32_le(proof_bytes, &mut offset)?;
    if inner_variant != 2 {
        return Err(PAError::UnsupportedProofType);
    }

    // Groth16Receipt<Claim> begins with `seal: Vec<u8>` as bincode bytes (u64 len + bytes).
    let seal_len = read_u64_le(proof_bytes, &mut offset)? as usize;
    let seal = read_bytes(proof_bytes, &mut offset, seal_len)?;
    Ok((selector, seal.to_vec()))
}

/// Extract the Groth16 seal bytes and selector from a bincode-serialized `InnerReceipt`.
///
/// Used for non-aggregated proofs (individual compliance/logic proofs).
/// The proof format is `InnerReceipt` directly without the `AggregationProof` wrapper.
///
/// Returns (selector, seal) where:
/// - selector: first 4 bytes of verifier_parameters (last 32 bytes of proof)
/// - seal: the 256-byte Groth16 proof
#[cfg(feature = "non-aggregated-proofs")]
pub fn extract_groth16_seal_from_inner_receipt(
    proof_bytes: &[u8],
) -> Result<(Selector, Vec<u8>), PAError> {
    let selector = extract_selector_from_tail(proof_bytes)?;

    // Parse InnerReceipt to extract seal
    let mut offset = 0usize;

    // InnerReceipt discriminant (Composite=0, Succinct=1, Groth16=2, Fake=3).
    let inner_variant = read_u32_le(proof_bytes, &mut offset)?;
    if inner_variant != 2 {
        return Err(PAError::UnsupportedProofType);
    }

    // Groth16Receipt begins with `seal: Vec<u8>` as bincode bytes (u64 len + bytes).
    let seal_len = read_u64_le(proof_bytes, &mut offset)? as usize;
    let seal = read_bytes(proof_bytes, &mut offset, seal_len)?;

    Ok((selector, seal.to_vec()))
}

/// Prepared proof data for risc0-solana verification.
#[derive(Clone)]
pub struct PreparedProof {
    pub proof: Proof,
    pub selector: Selector,
    pub image_id: [u8; 32],
    pub journal_digest: [u8; 32],
}

impl PreparedProof {
    /// Convert to a Seal for verifier_router CPI.
    pub fn to_seal(&self) -> Seal {
        Seal {
            selector: self.selector,
            proof: self.proof.clone(),
        }
    }
}

/// Prepare proof data for verification.
/// Extracts seal and selector, negates pi_a, computes journal digest, and selects image ID.
pub fn prepare_proof_for_verification(tx: &Transaction) -> Result<PreparedProof, PAError> {
    let proof_bytes = tx.aggregation_proof.as_ref().ok_or(PAError::InvalidProof)?;

    let (selector, seal_bytes) = extract_groth16_seal_from_aggregation_proof(proof_bytes)?;
    let seal = extract_seal(&seal_bytes)?;
    let negated_seal = negate_pi_a(&seal);

    let image_id = BATCH_AGGREGATION_IMAGE_ID;
    let journal_digest = crate::encoding::compute_batch_aggregation_journal_digest(tx)?.to_bytes();

    let mut pi_a = [0u8; 64];
    let mut pi_b = [0u8; 128];
    let mut pi_c = [0u8; 64];
    pi_a.copy_from_slice(&negated_seal[0..64]);
    pi_b.copy_from_slice(&negated_seal[64..192]);
    pi_c.copy_from_slice(&negated_seal[192..256]);

    Ok(PreparedProof {
        proof: Proof { pi_a, pi_b, pi_c },
        selector,
        image_id,
        journal_digest,
    })
}

/// Prepare an individual proof (raw seal) for verification.
///
/// For non-aggregated paths, each compliance and logic proof is verified individually.
/// The proof is expected to be a raw Groth16 seal (256 bytes) - receipt extraction
/// happens off-chain.
///
/// # Arguments
/// * `seal` - Raw Groth16 seal bytes (must be exactly 256 bytes)
/// * `selector` - The 4-byte selector identifying the verifier version
/// * `image_id` - The circuit's verifying key (image ID)
/// * `journal_digest` - SHA256 hash of the serialized instance
#[cfg(feature = "non-aggregated-proofs")]
pub fn prepare_individual_proof(
    seal: &[u8],
    selector: Selector,
    image_id: [u8; 32],
    journal_digest: [u8; 32],
) -> Result<PreparedProof, PAError> {
    if seal.len() != 256 {
        return Err(PAError::InvalidProof);
    }

    let mut seal_arr = [0u8; 256];
    seal_arr.copy_from_slice(seal);
    let negated_seal = negate_pi_a(&seal_arr);

    let mut pi_a = [0u8; 64];
    let mut pi_b = [0u8; 128];
    let mut pi_c = [0u8; 64];
    pi_a.copy_from_slice(&negated_seal[0..64]);
    pi_b.copy_from_slice(&negated_seal[64..192]);
    pi_c.copy_from_slice(&negated_seal[192..256]);

    Ok(PreparedProof {
        proof: Proof { pi_a, pi_b, pi_c },
        selector,
        image_id,
        journal_digest,
    })
}
