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
use crate::types::{Delta, Transaction};

use anchor_lang::solana_program::hash::hashv;
use anchor_lang::solana_program::secp256k1_recover::secp256k1_recover;
use solana_secp256k1::{Curve, UncompressedPoint};

/// Collect tags (nullifiers and commitments) in compliance unit order.
/// Returns tags as 32-byte arrays in the order: [nf0, cm0, nf1, cm1, ...]
pub fn collect_tags(tx: &Transaction) -> Result<Vec<[u8; 32]>, PAError> {
    let mut tags = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            tags.push(cu.instance.consumed_nullifier.to_bytes());
            tags.push(cu.instance.created_commitment.to_bytes());
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
/// This matches arm-risc0's `bytemuck::cast_slice` approach. The round-trip
/// bytes_to_words → words_to_bytes preserves the original byte order.
fn words_to_bytes(words: &[u32; 8]) -> [u8; 32] {
    *bytemuck::cast_ref(words)
}

/// Check if a 32-byte array is all zeros
fn is_zero(bytes: &[u8; 32]) -> bool {
    bytes.iter().all(|&b| b == 0)
}

/// Safe point addition that handles edge cases:
/// - Point doubling (P + P): uses the doubling formula
/// - Inverse points (P + (-P)): returns None (identity)
/// - Standard addition: delegates to UncompressedPoint::add
///
/// Returns None if the result is the identity point.
fn safe_point_add(
    p: &UncompressedPoint,
    q: &UncompressedPoint,
) -> Result<Option<UncompressedPoint>, PAError> {
    let x_p = p.x();
    let x_q = q.x();
    let y_p = p.y();
    let y_q = q.y();

    // Check if x-coordinates are equal
    if x_p == x_q {
        if y_p == y_q {
            // Point doubling: P + P
            // Use ecmul with scalar 2 for efficiency
            let two: [u8; 32] = {
                let mut arr = [0u8; 32];
                arr[31] = 2;
                arr
            };
            let doubled = Curve::ecmul(p, &two).map_err(|_| PAError::DeltaPointNotOnCurve)?;
            Ok(Some(doubled))
        } else {
            // Inverse points: P + (-P) = identity
            // (same x, different y means they're inverses on secp256k1)
            Ok(None)
        }
    } else {
        // Standard addition - safe to use the library's Add
        Ok(Some(*p + *q))
    }
}

/// Parse delta coordinates from a compliance instance and return as an UncompressedPoint.
///
/// Validates the point is on the secp256k1 curve (y² = x³ + 7 mod p).
/// The point (0, 0) is NOT on the curve and will error.
fn parse_delta_point(x_words: &[u32; 8], y_words: &[u32; 8]) -> Result<UncompressedPoint, PAError> {
    let x_bytes = words_to_bytes(x_words);
    let y_bytes = words_to_bytes(y_words);

    // (0, 0) is NOT a valid secp256k1 point - reject it
    if is_zero(&x_bytes) && is_zero(&y_bytes) {
        return Err(PAError::DeltaPointNotOnCurve);
    }

    // Validate point is on curve by computing valid y from x via lift_x.
    // lift_x returns the even y; the other valid y is p - y (odd).
    let valid_point =
        UncompressedPoint::lift_x(&x_bytes).map_err(|_| PAError::DeltaPointNotOnCurve)?;
    let valid_y = valid_point.y();

    // Check if provided y matches either valid y or its negation
    if y_bytes != valid_y {
        // Check negated y: p - y
        let mut negated = valid_point;
        negated.invert();
        if y_bytes != negated.y() {
            return Err(PAError::DeltaPointNotOnCurve);
        }
    }

    // Point is valid - construct UncompressedPoint
    let mut point_bytes = [0u8; 64];
    point_bytes[..32].copy_from_slice(&x_bytes);
    point_bytes[32..].copy_from_slice(&y_bytes);

    Ok(UncompressedPoint(point_bytes))
}

/// Accumulate delta points from all compliance instances using EC point addition.
/// Returns the accumulated point as an UncompressedPoint, or None if the result is the identity.
pub fn accumulate_deltas(tx: &Transaction) -> Result<Option<UncompressedPoint>, PAError> {
    let mut accumulated: Option<UncompressedPoint> = None;

    for action in &tx.actions {
        for cu in &action.compliance_units {
            // Direct struct access (ComplianceInstance is now a direct field, not bytes)
            let point = parse_delta_point(&cu.instance.delta_x, &cu.instance.delta_y)?;

            match accumulated {
                None => {
                    // First point
                    accumulated = Some(point);
                }
                Some(acc) => {
                    // Use safe_point_add which handles:
                    // - Point doubling (P + P)
                    // - Inverse points (P + (-P) = identity)
                    accumulated = safe_point_add(&acc, &point)?;
                }
            }
        }
    }

    Ok(accumulated)
}

/// Convert an uncompressed point to its "address" representation using Solana syscall.
/// address = last 20 bytes of SHA-256(x || y)
fn point_to_address(point: &UncompressedPoint) -> [u8; 20] {
    // UncompressedPoint stores 64 bytes (x || y) in big-endian
    let hash: [u8; 32] = hashv(&[&point.0]).to_bytes();

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
