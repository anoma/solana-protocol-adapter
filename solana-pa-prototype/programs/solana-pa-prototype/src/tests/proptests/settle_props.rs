use super::strategies::arb_action;
use crate::settle::{extract_commitments, extract_nullifiers, total_resource_count};
use arm_core::aggregation_instance::AggregationInstance;
use arm_core::Digest;
use proptest::prelude::*;

fn instance_from_actions(
    actions: Vec<arm_core::aggregation_instance::ActionAggregated>,
) -> AggregationInstance {
    AggregationInstance {
        compliance_key: Digest::from_bytes([0u8; 32]),
        kind_table_commitment: Digest::from_bytes([0u8; 32]),
        actions,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_extract_preserves_count_and_order(
        actions in prop::collection::vec(arb_action(), 1..6),
    ) {
        let instance = instance_from_actions(actions.clone());
        let nullifiers = extract_nullifiers(&instance);
        let commitments = extract_commitments(&instance);

        let expected_nullifiers: Vec<Digest> = actions
            .iter()
            .flat_map(|a| a.consumed_publics.iter().map(|c| c.resource_nullifier))
            .collect();
        let expected_commitments: Vec<Digest> = actions
            .iter()
            .flat_map(|a| a.created_publics.iter().map(|c| c.resource_commitment))
            .collect();

        prop_assert_eq!(nullifiers, expected_nullifiers,
            "nullifiers must be all consumed resources in instance order");
        prop_assert_eq!(commitments, expected_commitments,
            "commitments must be all created resources in instance order");
        prop_assert_eq!(
            total_resource_count(&instance),
            instance.actions.iter()
                .map(|a| a.consumed_publics.len() + a.created_publics.len())
                .sum::<usize>()
        );
    }
}
