use super::strategies::arb_action;
use crate::settle::{extract_commitments, extract_nullifiers};
use crate::tests::utils::minimal_instance;
use arm_core::aggregation_instance::AggregationInstance;
use arm_core::Digest;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_extract_preserves_count_and_order(
        actions in prop::collection::vec(arb_action(), 1..6),
    ) {
        let instance = AggregationInstance {
            actions: actions.clone(),
            ..minimal_instance()
        };
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
    }
}
