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
use dashu::integer::UBig;
use solana_secp256k1::{Curve, Secp256k1Point, UncompressedPoint};

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
/// arm-risc0 uses `bytemuck::cast_slice` for words_to_bytes, which means:
/// - Words are stored in native (little-endian) byte order
/// - The 8 words map directly to 32 bytes via bytemuck
fn words_to_bytes(words: &[u32; 8]) -> [u8; 32] {
    // bytemuck cast - same as arm-risc0's words_to_bytes
    *bytemuck::cast_ref(words)
}

/// Parse delta coordinates from a compliance instance and return as an UncompressedPoint.
///
/// All compliance units must provide valid secp256k1 curve points. The point (0, 0)
/// is NOT on the curve and will error - the identity point (point at infinity) has
/// no valid affine representation.
fn parse_delta_point(x_words: &[u32; 8], y_words: &[u32; 8]) -> Result<UncompressedPoint, PAError> {
    let x_bytes = words_to_bytes(x_words);
    let y_bytes = words_to_bytes(y_words);

    // solana-secp256k1 does not validate that points lie on the curve.
    // We must check the curve equation manually: y² ≡ x³ + 7 (mod p)
    let p = UBig::from_be_bytes(&Curve::P);
    let x = UBig::from_be_bytes(&x_bytes);
    let y = UBig::from_be_bytes(&y_bytes);

    let y_squared = y.sqr() % &p;
    let x_cubed_plus_7 = (x.cubic() + UBig::from_word(7)) % &p;

    if y_squared != x_cubed_plus_7 {
        return Err(PAError::DeltaPointNotOnCurve);
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
    // UncompressedPoint has no identity representation, so we track it with Option.
    let mut accumulated: Option<UncompressedPoint> = None;

    for action in &tx.actions {
        for cu in &action.compliance_units {
            let point = parse_delta_point(&cu.instance.delta_x, &cu.instance.delta_y)?;

            accumulated = match accumulated {
                None => Some(point),
                Some(acc) => {
                    // solana-secp256k1's Add does not handle point doubling (P + P)
                    // or inverse points (P + (-P)). We detect and handle these cases.
                    let x_acc = acc.x();
                    let x_pt = point.x();

                    if x_acc == x_pt {
                        // Same x-coordinate: either doubling or inverse
                        if acc.y() == point.y() {
                            // Point doubling: P + P - use scalar multiplication by 2
                            #[rustfmt::skip]
                            let two: [u8; 32] = [
                                0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,
                                0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,2
                            ];
                            Some(
                                Curve::ecmul(&acc, &two)
                                    .map_err(|_| PAError::DeltaPointNotOnCurve)?,
                            )
                        } else {
                            // Inverse points: P + (-P) = identity
                            None
                        }
                    } else {
                        // Standard case - library's Add is safe here
                        Some(acc + point)
                    }
                }
            };
        }
    }

    Ok(accumulated)
}

/// Convert an uncompressed point to its "address" representation using Solana syscall.
/// address = last 20 bytes of SHA-256(x || y)
fn point_to_address(point: &UncompressedPoint) -> [u8; 20] {
    // UncompressedPoint stores 64 bytes (x || y)
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
