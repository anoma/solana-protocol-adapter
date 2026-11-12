//! Settlement helpers over the aggregation instance.
//!
//! The aggregation instance is the only proof-backed source of settlement
//! data: every field below is committed by the batch aggregation Groth16
//! proof via the journal digest, so reading from it (never from the
//! transaction's unproven parts) is what makes the extracted values
//! trustworthy.

use anchor_lang::prelude::Result;
use arm_core::aggregation_instance::{
    ActionAggregated, AggregationInstance, AggregationResourceInstance,
};

use crate::ActionExecuted;

fn resources(instance: &AggregationInstance) -> impl Iterator<Item = &AggregationResourceInstance> {
    instance.actions.iter().flat_map(|a| &a.resource_instances)
}

/// The number of consumed resources: the nullifier markers a settlement creates.
pub fn nullifier_count(instance: &AggregationInstance) -> usize {
    resources(instance).filter(|r| r.is_consumed()).count()
}

/// The number of resources a settlement consumes or creates.
pub fn resource_count(instance: &AggregationInstance) -> usize {
    instance
        .actions
        .iter()
        .map(|a| a.resource_instances.len())
        .sum()
}

/// pa-evm's `_processAction`: runs `process_resource` on each of the
/// action's resources, in the order of `resource_instances`, and returns the
/// action's `ActionExecuted`, whose arrays hold the consumed and the created
/// resources each in that order. `process_resource` is where a resource's
/// checks, its nullifier or commitment, its forwarder calls and its payload
/// events happen, so all of them follow the list.
pub fn process_action(
    action: &ActionAggregated,
    mut process_resource: impl FnMut(&AggregationResourceInstance) -> Result<()>,
) -> Result<ActionExecuted> {
    let consumed_count = action
        .resource_instances
        .iter()
        .filter(|r| r.is_consumed())
        .count();
    let created_count = action.resource_instances.len() - consumed_count;
    let mut executed = ActionExecuted {
        action_tree_root: action.action_tree_root.into(),
        nullifiers: Vec::with_capacity(consumed_count),
        consumed_logic_refs: Vec::with_capacity(consumed_count),
        commitments: Vec::with_capacity(created_count),
        created_logic_refs: Vec::with_capacity(created_count),
    };
    for resource in &action.resource_instances {
        process_resource(resource)?;
        let (tags, logic_refs) = if resource.is_consumed() {
            (&mut executed.nullifiers, &mut executed.consumed_logic_refs)
        } else {
            (&mut executed.commitments, &mut executed.created_logic_refs)
        };
        tags.push(resource.tag().into());
        logic_refs.push(resource.logic_ref().into());
    }
    Ok(executed)
}
