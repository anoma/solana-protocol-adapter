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

/// The number of created resources: the commitments a settlement appends.
pub fn commitment_count(instance: &AggregationInstance) -> usize {
    resources(instance).filter(|r| !r.is_consumed()).count()
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
        match resource {
            AggregationResourceInstance::Consumed {
                nullifier,
                logic_ref,
                ..
            } => {
                executed.nullifiers.push((*nullifier).into());
                executed.consumed_logic_refs.push((*logic_ref).into());
            }
            AggregationResourceInstance::Created {
                commitment,
                logic_ref,
                ..
            } => {
                executed.commitments.push((*commitment).into());
                executed.created_logic_refs.push((*logic_ref).into());
            }
        }
    }
    Ok(executed)
}
