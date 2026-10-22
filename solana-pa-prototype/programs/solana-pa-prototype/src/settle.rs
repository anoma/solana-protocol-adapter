//! Settlement extraction helpers over the aggregation instance.
//!
//! The aggregation instance is the only proof-backed source of settlement
//! data: every field below is committed by the batch aggregation Groth16
//! proof via the journal digest, so reading from it (never from the
//! transaction's unproven parts) is what makes the extracted values
//! trustworthy.

use arm_core::aggregation_instance::{
    ActionAggregated, AggregationInstance, ConsumedResourceAggregated, CreatedResourceAggregated,
};
use arm_core::logic_instance::AppData;
use arm_core::Digest;

/// One resource of an action, seen uniformly across the consumed/created split.
pub struct ResourceView<'a> {
    /// Nullifier of a consumed resource, commitment of a created one.
    pub tag: Digest,
    pub logic_ref: Digest,
    pub app_data: &'a AppData,
    pub is_consumed: bool,
}

/// An action's resources in the order the aggregation journal commits to: every
/// consumed resource, then every created one. Each on-chain effect sequence
/// (events, external calls) is produced by traversing this, so the order the
/// effects follow has a single definition.
pub fn action_resources(action: &ActionAggregated) -> impl Iterator<Item = ResourceView<'_>> {
    action
        .consumed_publics
        .iter()
        .map(|consumed| ResourceView {
            tag: consumed.resource_nullifier,
            logic_ref: consumed.resource_logic_ref,
            app_data: &consumed.app_data,
            is_consumed: true,
        })
        .chain(action.created_publics.iter().map(|created| ResourceView {
            tag: created.resource_commitment,
            logic_ref: created.resource_logic_ref,
            app_data: &created.app_data,
            is_consumed: false,
        }))
}

fn consumed(instance: &AggregationInstance) -> impl Iterator<Item = &ConsumedResourceAggregated> {
    instance.actions.iter().flat_map(|a| &a.consumed_publics)
}

fn created(instance: &AggregationInstance) -> impl Iterator<Item = &CreatedResourceAggregated> {
    instance.actions.iter().flat_map(|a| &a.created_publics)
}

/// The number of consumed resources: the nullifier markers a settlement creates.
pub fn nullifier_count(instance: &AggregationInstance) -> usize {
    consumed(instance).count()
}

/// The number of created resources: the commitments a settlement appends.
pub fn commitment_count(instance: &AggregationInstance) -> usize {
    created(instance).count()
}

/// Deduplicated commitment-tree roots consumed by the instance.
///
/// Each root is validated against the adapter's historical root set, and each
/// `is_root_valid` call may invoke `Pubkey::find_program_address` (~1500 CU),
/// so deduplication saves significant compute when resources share roots.
/// Pre-sized to the worst case (one root per consumed resource).
pub fn unique_consumed_roots(instance: &AggregationInstance) -> Vec<Digest> {
    let mut unique_roots: Vec<Digest> = Vec::with_capacity(consumed(instance).count());
    for resource in consumed(instance) {
        if !unique_roots.contains(&resource.commitment_tree_root) {
            unique_roots.push(resource.commitment_tree_root);
        }
    }
    unique_roots
}
