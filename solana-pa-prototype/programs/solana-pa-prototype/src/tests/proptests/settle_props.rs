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
        let resources = || actions.iter().flat_map(|a| &a.resource_instances);
        let consumed = resources().filter(|r| r.is_consumed()).count();
        let created = resources().filter(|r| !r.is_consumed()).count();
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
