//! Delta proof verification for balance conservation.
//!
//! The delta proof ensures that a transaction is balanced - the total value
//! created equals the total value consumed. This is enforced via an ECDSA
//! signature over the accumulated delta points from all compliance instances.
//!
//! Algorithm:
//! 1. Collect tags (nullifiers and commitments) in compliance unit order
//! 2. Compute verifying key = SHA-256(tags) using Solana syscall
//! 3. Accumulate delta points via secp256k1 EC point addition
//! 4. Verify ECDSA signature using Solana secp256k1_recover syscall

use crate::error::PAError;
use crate::journal::parse_compliance_instance;
use crate::types::{Delta, Transaction};

use anchor_lang::solana_program::hash::hashv;
use anchor_lang::solana_program::secp256k1_recover::secp256k1_recover;
use k256::elliptic_curve::group::prime::PrimeCurveAffine;
use k256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use k256::{AffinePoint, EncodedPoint, ProjectivePoint};

/// Collect tags (nullifiers and commitments) in compliance unit order.
/// Returns tags as 32-byte arrays in the order: [nf0, cm0, nf1, cm1, ...]
pub fn collect_tags(tx: &Transaction) -> Result<Vec<[u8; 32]>, PAError> {
    let mut tags = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)
                .map_err(|_| PAError::InvalidTransactionData)?;
            tags.push(instance.consumed_nullifier.to_bytes());
            tags.push(instance.created_commitment.to_bytes());
        }
    }
    Ok(tags)
}

/// Compute verifying key = SHA-256(concatenated tags) using Solana syscall.
pub fn compute_verifying_key(tags: &[[u8; 32]]) -> [u8; 32] {
    let refs: Vec<&[u8]> = tags.iter().map(|t| t.as_slice()).collect();
    hashv(&refs).to_bytes()
}

/// Convert [u32; 8] words to [u8; 32] bytes.
///
/// arm-risc0 uses `bytemuck::cast_slice` for words_to_bytes, which means:
/// - Words are stored in native (little-endian) byte order
/// - The 8 words map directly to 32 bytes via bytemuck
fn words_to_bytes(words: &[u32; 8]) -> [u8; 32] {
    // bytemuck cast - same as arm-risc0's words_to_bytes
    *bytemuck::cast_ref(words)
}

/// Parse delta coordinates from a compliance instance and return as a ProjectivePoint.
/// Returns None if the point is the identity (0, 0).
fn parse_delta_point(
    x_words: &[u32; 8],
    y_words: &[u32; 8],
) -> Result<Option<ProjectivePoint>, PAError> {
    let x_bytes = words_to_bytes(x_words);
    let y_bytes = words_to_bytes(y_words);

    // Check if this is the identity point (0, 0)
    if x_bytes == [0u8; 32] && y_bytes == [0u8; 32] {
        return Ok(None);
    }

    // Construct encoded point from affine coordinates (uncompressed format)
    let encoded_point = EncodedPoint::from_affine_coordinates(
        (&x_bytes).into(),
        (&y_bytes).into(),
        false, // uncompressed
    );

    // Convert to ProjectivePoint, validating the point is on the curve
    let point = ProjectivePoint::from_encoded_point(&encoded_point)
        .into_option()
        .ok_or(PAError::DeltaPointNotOnCurve)?;

    Ok(Some(point))
}

/// Accumulate delta points from all compliance instances using EC point addition.
/// Returns the accumulated point as an AffinePoint, or None if the result is the identity.
pub fn accumulate_deltas(tx: &Transaction) -> Result<Option<AffinePoint>, PAError> {
    let mut accumulated = ProjectivePoint::IDENTITY;

    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)
                .map_err(|_| PAError::InvalidTransactionData)?;

            if let Some(point) = parse_delta_point(&instance.delta_x, &instance.delta_y)? {
                accumulated += point;
            }
        }
    }

    // Convert to affine and check for identity
    let affine = accumulated.to_affine();
    if affine.is_identity().into() {
        Ok(None)
    } else {
        Ok(Some(affine))
    }
}

/// Convert an affine point to its "address" representation using Solana syscall.
/// address = last 20 bytes of SHA-256(x || y)
fn point_to_address(point: &AffinePoint) -> [u8; 20] {
    let encoded = point.to_encoded_point(false);
    let x_bytes = encoded.x().expect("non-identity point has x coordinate");
    let y_bytes = encoded.y().expect("non-identity point has y coordinate");

    let hash: [u8; 32] = hashv(&[x_bytes, y_bytes]).to_bytes();

    // Take last 20 bytes
    let mut address = [0u8; 20];
    address.copy_from_slice(&hash[12..32]);
    address
}

/// Verify the delta proof using Solana syscalls for optimal CU usage.
///
/// The delta proof is an ECDSA signature that proves the transaction is balanced.
/// The signer's public key must correspond to the accumulated delta point.
pub fn verify_delta_proof(tx: &Transaction) -> Result<(), PAError> {
    // 1. Check delta_proof type FIRST to avoid expensive operations in Witness mode
    let signature_bytes = match &tx.delta_proof {
        Delta::Proof(bytes) => {
            if bytes.len() != 65 {
                return Err(PAError::InvalidDeltaProof);
            }
            bytes.as_slice()
        }
        Delta::Witness(_) => {
            // Witness mode is not supported - always require a proof
            // (matches arm-risc0 behavior: transaction.rs:90)
            return Err(PAError::ExpectedDeltaProof);
        }
    };

    // 2. Collect tags
    let tags = collect_tags(tx)?;

    // Empty transaction - no delta verification needed
    if tags.is_empty() {
        return Ok(());
    }

    // 3. Compute verifying key (message hash) using Solana syscall
    let verifying_key = compute_verifying_key(&tags);

    // 4. Accumulate delta points
    let accumulated = accumulate_deltas(tx)?;

    // 5. Parse signature (64 bytes) and recovery ID (1 byte)
    let sig_bytes: [u8; 64] = signature_bytes[0..64]
        .try_into()
        .map_err(|_| PAError::InvalidDeltaProof)?;

    let recid_byte = signature_bytes[64];
    // Convert from Ethereum format (27/28) to recovery ID (0/1)
    let recid = if recid_byte >= 27 {
        recid_byte - 27
    } else {
        recid_byte
    };

    if recid > 3 {
        return Err(PAError::InvalidDeltaProof);
    }

    // 6. Recover the public key using Solana syscall (much cheaper than software recovery)
    let recovered_pubkey = secp256k1_recover(&verifying_key, recid, &sig_bytes)
        .map_err(|_| PAError::DeltaProofVerificationFailed)?;

    // 7. Convert recovered key to address using Solana syscall
    // secp256k1_recover returns 64 bytes (x || y), no 0x04 prefix
    let recovered_hash: [u8; 32] = hashv(&[&recovered_pubkey.0]).to_bytes();
    let recovered_address: [u8; 20] = recovered_hash[12..32].try_into().unwrap();

    // 8. Compare with expected address from accumulated delta
    let expected_address = match accumulated {
        Some(point) => point_to_address(&point),
        None => {
            // Identity point - the "zero address"
            // For a balanced transaction with all deltas canceling out,
            // the accumulated point is the identity
            [0u8; 20]
        }
    };

    if recovered_address != expected_address {
        return Err(PAError::DeltaMismatch);
    }

    Ok(())
}
