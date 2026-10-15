use super::strategies::arb_action;
use crate::settle::{commitment_count, nullifier_count};
use crate::tests::utils::minimal_instance;
use arm_core::aggregation_instance::AggregationInstance;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_counts_are_the_consumed_and_created_resources(
        actions in prop::collection::vec(arb_action(), 1..6),
    ) {
        let consumed: usize = actions.iter().map(|a| a.consumed_publics.len()).sum();
        let created: usize = actions.iter().map(|a| a.created_publics.len()).sum();
        let instance = AggregationInstance {
            actions,
            ..minimal_instance()
        };

        prop_assert_eq!(nullifier_count(&instance), consumed,
            "one nullifier marker per consumed resource, across all actions");
        prop_assert_eq!(commitment_count(&instance), created,
            "one commitment per created resource, across all actions");
    }
}
