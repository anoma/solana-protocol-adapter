use crate::types::{OutputMode, SolanaExternalCall};
use arm_core::aggregation_instance::{ActionAggregated, AggregationResourceInstance};
use arm_core::logic_instance::AppData;
use arm_core::Digest;
use proptest::prelude::*;

pub fn arb_digest() -> impl Strategy<Value = Digest> {
    prop::array::uniform32(any::<u8>()).prop_map(Digest::from_bytes)
}

pub fn arb_byte_vec(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max_len)
}

pub fn arb_consumed_resource() -> impl Strategy<Value = AggregationResourceInstance> {
    (arb_digest(), arb_digest(), arb_digest()).prop_map(|(nullifier, logic_ref, root)| {
        AggregationResourceInstance::Consumed {
            nullifier,
            logic_ref,
            commitment_tree_root: root,
            app_data: AppData::default(),
        }
    })
}

pub fn arb_created_resource() -> impl Strategy<Value = AggregationResourceInstance> {
    (arb_digest(), arb_digest()).prop_map(|(commitment, logic_ref)| {
        AggregationResourceInstance::Created {
            commitment,
            logic_ref,
            app_data: AppData::default(),
        }
    })
}

/// An action whose consumed and created resources come in any order.
pub fn arb_action() -> impl Strategy<Value = ActionAggregated> {
    (
        prop::collection::vec(
            prop_oneof![arb_consumed_resource(), arb_created_resource()],
            0..8,
        ),
        prop::array::uniform8(any::<u32>()),
        prop::array::uniform8(any::<u32>()),
        arb_digest(),
    )
        .prop_map(|(resource_instances, dx, dy, root)| ActionAggregated {
            resource_instances,
            delta_x: dx,
            delta_y: dy,
            action_tree_root: root,
        })
}

pub fn arb_solana_external_call(max_data_len: usize) -> impl Strategy<Value = SolanaExternalCall> {
    (
        prop::array::uniform32(any::<u8>()),
        arb_byte_vec(max_data_len),
        arb_byte_vec(max_data_len),
        1..=10u8,
    )
        .prop_map(|(pid, input, output, num_accounts)| SolanaExternalCall {
            program_id: pid,
            instruction_data: input,
            expected_output: output,
            output_mode: OutputMode::ReturnData,
            num_accounts,
        })
}
