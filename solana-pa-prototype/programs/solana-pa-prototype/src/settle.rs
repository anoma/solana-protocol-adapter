//! Settlement extraction helpers over the aggregation instance.
//!
//! The aggregation instance is the only proof-backed source of settlement
//! data: every field below is committed by the batch aggregation Groth16
//! proof via the journal digest, so reading from it (never from the
//! transaction's unproven parts) is what makes the extracted values
//! trustworthy.

use arm_core::aggregation_instance::AggregationInstance;
use arm_core::Digest;

/// Total number of resources (consumed + created) across the instance.
pub fn total_resource_count(instance: &AggregationInstance) -> usize {
    instance
        .actions
        .iter()
        .map(|a| a.consumed_publics.len() + a.created_publics.len())
        .sum()
}

/// Nullifiers of all consumed resources, in instance order.
/// Pre-sized so the BPF bump allocator (no free) doesn't retain
/// capacity-doubled buffers.
pub fn extract_nullifiers(instance: &AggregationInstance) -> Vec<Digest> {
    let total: usize = instance
        .actions
        .iter()
        .map(|a| a.consumed_publics.len())
        .sum();
    let mut out = Vec::with_capacity(total);
    for action in &instance.actions {
        for consumed in &action.consumed_publics {
            out.push(consumed.resource_nullifier);
        }
    }
    out
}

/// Commitments of all created resources, in instance order.
pub fn extract_commitments(instance: &AggregationInstance) -> Vec<Digest> {
    let total: usize = instance
        .actions
        .iter()
        .map(|a| a.created_publics.len())
        .sum();
    let mut out = Vec::with_capacity(total);
    for action in &instance.actions {
        for created in &action.created_publics {
            out.push(created.resource_commitment);
        }
    }
    out
}

/// Deduplicated commitment-tree roots consumed by the instance.
///
/// Each root is validated against the adapter's historical root set, and each
/// `is_root_valid` call may invoke `Pubkey::find_program_address` (~1500 CU),
/// so deduplication saves significant compute when resources share roots.
/// Pre-sized to the worst case (one root per consumed resource).
pub fn unique_consumed_roots(instance: &AggregationInstance) -> Vec<Digest> {
    let total: usize = instance
        .actions
        .iter()
        .map(|a| a.consumed_publics.len())
        .sum();
    let mut unique_roots: Vec<Digest> = Vec::with_capacity(total);
    for action in &instance.actions {
        for consumed in &action.consumed_publics {
            if !unique_roots.contains(&consumed.commitment_tree_root) {
                unique_roots.push(consumed.commitment_tree_root);
            }
        }
    }
    unique_roots
}
