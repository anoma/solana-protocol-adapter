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
use crate::journal::parse_compliance_instance;

use anchor_lang::solana_program::hash::hashv;
use anchor_lang::solana_program::secp256k1_recover::secp256k1_recover;
use libsecp256k1::curve::{Affine, Field, Jacobian};

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

/// Parse a 32-byte big-endian array into a libsecp256k1 Field element.
fn bytes_to_field(bytes: &[u8; 32]) -> Result<Field, PAError> {
    let mut fe = Field::default();
    if !fe.set_b32(bytes) {
        return Err(PAError::DeltaPointNotOnCurve);
    }
    Ok(fe)
}

/// Accumulate delta points from all compliance instances using EC point addition.
/// Returns the accumulated point, or None if the result is the identity (infinity).
pub fn accumulate_deltas(tx: &Transaction) -> Result<Option<Affine>, PAError> {
    let mut accumulated = Jacobian::default();
    accumulated.set_infinity();

    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = parse_compliance_instance(&cu.instance)
                .map_err(|_| PAError::InvalidTransactionData)?;

            // Convert delta coordinates to bytes (matching arm-risc0 encoding)
            let x_bytes = words_to_bytes(&instance.delta_x);
            let y_bytes = words_to_bytes(&instance.delta_y);

            // Check if this is the identity point (0, 0)
            if x_bytes == [0u8; 32] && y_bytes == [0u8; 32] {
                // Identity point contributes nothing
                continue;
            }

            // Parse coordinates as field elements
            let x = bytes_to_field(&x_bytes)?;
            let y = bytes_to_field(&y_bytes)?;

            // Create affine point and verify it's on the curve
            let mut affine = Affine::default();
            affine.set_xy(&x, &y);
            if !affine.is_valid_var() {
                return Err(PAError::DeltaPointNotOnCurve);
            }

            // Add to accumulated point
            accumulated = accumulated.add_ge(&affine);
        }
    }

    // Convert back to affine
    if accumulated.is_infinity() {
        Ok(None)
    } else {
        let mut result = Affine::default();
        result.set_gej(&accumulated);
        Ok(Some(result))
    }
}

/// Convert an affine point to its "address" representation using Solana syscall.
/// address = last 20 bytes of SHA-256(x || y)
fn point_to_address(point: &Affine) -> [u8; 20] {
    let mut x_bytes = [0u8; 32];
    let mut y_bytes = [0u8; 32];
    let mut x = point.x;
    let mut y = point.y;
    x.normalize_var();
    y.normalize_var();
    x.fill_b32(&mut x_bytes);
    y.fill_b32(&mut y_bytes);

    let hash: [u8; 32] = hashv(&[&x_bytes, &y_bytes]).to_bytes();

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

    // 4. Accumulate delta points (still uses libsecp256k1 for EC math)
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

    // 6. Recover the public key using Solana syscall (much cheaper than libsecp256k1)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;
    use crate::merkle::EMPTY_TREE_ROOT;

    fn create_test_instance(nullifier: [u8; 32], commitment: [u8; 32]) -> ComplianceInstance {
        let empty_tree_root = EMPTY_TREE_ROOT;
        ComplianceInstance {
            consumed_nullifier: Digest::from_bytes(nullifier),
            consumed_logic_ref: Digest::default(),
            consumed_commitment_tree_root: empty_tree_root,
            created_commitment: Digest::from_bytes(commitment),
            created_logic_ref: Digest::default(),
            delta_x: [0u32; 8],
            delta_y: [0u32; 8],
        }
    }

    #[test]
    fn test_collect_tags_order() {
        let instance1 = create_test_instance([1u8; 32], [2u8; 32]);
        let instance2 = create_test_instance([3u8; 32], [4u8; 32]);

        let tx = Transaction {
            actions: vec![Action {
                compliance_units: vec![
                    ComplianceUnit {
                        instance: bincode::serialize(&instance1).unwrap(),
                        proof: None,
                    },
                    ComplianceUnit {
                        instance: bincode::serialize(&instance2).unwrap(),
                        proof: None,
                    },
                ],
                logic_verifier_inputs: vec![],
            }],
            delta_proof: Delta::Witness(vec![]),
            expected_balance: None,
            aggregation_proof: None,
        };

        let tags = collect_tags(&tx).unwrap();
        assert_eq!(tags.len(), 4);
        assert_eq!(tags[0], [1u8; 32]); // nf1
        assert_eq!(tags[1], [2u8; 32]); // cm1
        assert_eq!(tags[2], [3u8; 32]); // nf2
        assert_eq!(tags[3], [4u8; 32]); // cm2
    }

    #[test]
    fn test_compute_verifying_key_deterministic() {
        let tags = vec![[1u8; 32], [2u8; 32]];
        let vk1 = compute_verifying_key(&tags);
        let vk2 = compute_verifying_key(&tags);
        assert_eq!(vk1, vk2);
    }

    #[test]
    fn test_accumulate_deltas_identity() {
        // Transaction with zero deltas should result in identity
        let instance = create_test_instance([1u8; 32], [2u8; 32]);

        let tx = Transaction {
            actions: vec![Action {
                compliance_units: vec![ComplianceUnit {
                    instance: bincode::serialize(&instance).unwrap(),
                    proof: None,
                }],
                logic_verifier_inputs: vec![],
            }],
            delta_proof: Delta::Witness(vec![]),
            expected_balance: None,
            aggregation_proof: None,
        };

        let result = accumulate_deltas(&tx).unwrap();
        assert!(result.is_none()); // Identity point
    }

    #[test]
    fn test_verify_delta_proof_witness_rejected() {
        // Witness mode is rejected - must provide actual proof
        let instance = create_test_instance([1u8; 32], [2u8; 32]);

        let tx = Transaction {
            actions: vec![Action {
                compliance_units: vec![ComplianceUnit {
                    instance: bincode::serialize(&instance).unwrap(),
                    proof: None,
                }],
                logic_verifier_inputs: vec![],
            }],
            delta_proof: Delta::Witness(vec![]),
            expected_balance: None,
            aggregation_proof: None,
        };

        // Should fail - witness mode is not allowed
        let result = verify_delta_proof(&tx);
        assert!(result.is_err());
    }

    #[test]
    fn test_words_to_bytes() {
        // Test conversion matches arm-risc0's bytemuck-based words_to_bytes
        // Word 0x04030201 stored in little-endian memory = bytes [0x01, 0x02, 0x03, 0x04]
        let words: [u32; 8] = [0x04030201, 0x08070605, 0x0c0b0a09, 0x100f0e0d,
                               0x14131211, 0x18171615, 0x1c1b1a19, 0x201f1e1d];
        let bytes = words_to_bytes(&words);

        // bytemuck casts directly, so word 0 (0x04030201) becomes first 4 bytes
        // in little-endian order: [0x01, 0x02, 0x03, 0x04]
        assert_eq!(bytes[0..4], [0x01, 0x02, 0x03, 0x04]);
        assert_eq!(bytes[4..8], [0x05, 0x06, 0x07, 0x08]);
    }
}
