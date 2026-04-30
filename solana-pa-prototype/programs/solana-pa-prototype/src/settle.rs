//! Settlement extraction helpers for processing RM transactions.
//!
//! ComplianceUnit::instance is journal bytes on the wire (224 bytes per CU,
//! laid out as 7 contiguous 32-byte fields). The on-chain BPF heap is small
//! and easily exhausted by allocating a structured `ComplianceInstance` per
//! CU, so the readers below pull individual fields straight out of the byte
//! slice without intermediate allocation.

use crate::error::PAError;
use arm_core::compliance_unit::ComplianceUnit;
use arm_core::transaction::Transaction;
use arm_core::Digest;

/// Byte offsets of each `ComplianceInstance` field within its journal bytes,
/// matching the `arm_core::compliance::ComplianceInstance` Borsh layout
/// (u32-word array, contiguous):
///   words 0..8   — consumed_nullifier
///   words 8..16  — consumed_logic_ref
///   words 16..24 — consumed_commitment_tree_root
///   words 24..32 — created_commitment
///   words 32..40 — created_logic_ref
///   words 40..48 — delta_x
///   words 48..56 — delta_y
const CONSUMED_NULLIFIER_OFFSET: usize = 0;
const CONSUMED_LOGIC_REF_OFFSET: usize = 32;
const CONSUMED_ROOT_OFFSET: usize = 64;
const CREATED_COMMITMENT_OFFSET: usize = 96;
const CREATED_LOGIC_REF_OFFSET: usize = 128;
const COMPLIANCE_INSTANCE_BYTES: usize = 224;

fn read_field(instance: &[u8], offset: usize) -> Result<Digest, PAError> {
    let chunk: [u8; 32] = instance
        .get(offset..offset + 32)
        .ok_or(PAError::ComplianceInstanceParseFailed)?
        .try_into()
        .map_err(|_| PAError::ComplianceInstanceParseFailed)?;
    Ok(Digest::from_bytes(chunk))
}

/// Sanity-check that a CU's journal bytes are at least full ComplianceInstance
/// size before letting callers index into them.
fn check_instance_len(cu: &ComplianceUnit) -> Result<(), PAError> {
    if cu.instance.len() < COMPLIANCE_INSTANCE_BYTES {
        return Err(PAError::ComplianceInstanceParseFailed);
    }
    Ok(())
}

pub fn read_consumed_nullifier(cu: &ComplianceUnit) -> Result<Digest, PAError> {
    check_instance_len(cu)?;
    read_field(&cu.instance, CONSUMED_NULLIFIER_OFFSET)
}

pub fn read_consumed_logic_ref(cu: &ComplianceUnit) -> Result<Digest, PAError> {
    check_instance_len(cu)?;
    read_field(&cu.instance, CONSUMED_LOGIC_REF_OFFSET)
}

pub fn read_consumed_root(cu: &ComplianceUnit) -> Result<Digest, PAError> {
    check_instance_len(cu)?;
    read_field(&cu.instance, CONSUMED_ROOT_OFFSET)
}

pub fn read_created_commitment(cu: &ComplianceUnit) -> Result<Digest, PAError> {
    check_instance_len(cu)?;
    read_field(&cu.instance, CREATED_COMMITMENT_OFFSET)
}

pub fn read_created_logic_ref(cu: &ComplianceUnit) -> Result<Digest, PAError> {
    check_instance_len(cu)?;
    read_field(&cu.instance, CREATED_LOGIC_REF_OFFSET)
}

fn total_compliance_units(tx: &Transaction) -> usize {
    tx.actions.iter().map(|a| a.compliance_units.len()).sum()
}

/// Extract nullifiers across all actions, reading directly from journal bytes.
/// Pre-sized to the exact number of CUs so the BPF bump allocator doesn't
/// retain capacity-doubled buffers from `.collect()`'s growth.
pub fn extract_nullifiers(tx: &Transaction) -> Result<Vec<Digest>, PAError> {
    let mut out = Vec::with_capacity(total_compliance_units(tx));
    for action in &tx.actions {
        for cu in &action.compliance_units {
            out.push(read_consumed_nullifier(cu)?);
        }
    }
    Ok(out)
}

/// Extract commitments across all actions, reading directly from journal bytes.
pub fn extract_commitments(tx: &Transaction) -> Result<Vec<Digest>, PAError> {
    let mut out = Vec::with_capacity(total_compliance_units(tx));
    for action in &tx.actions {
        for cu in &action.compliance_units {
            out.push(read_created_commitment(cu)?);
        }
    }
    Ok(out)
}
