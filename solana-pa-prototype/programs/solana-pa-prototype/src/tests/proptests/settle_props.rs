//! Property tests for settlement extraction.

use proptest::prelude::*;
use crate::settle::{extract_nullifiers, extract_commitments};
use super::strategies::{arb_compliance_instance, build_tx_from_instances};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: extract_nullifiers preserves CU order.
    #[test]
    fn prop_extract_nullifiers_order(
        inst1 in arb_compliance_instance(),
        inst2 in arb_compliance_instance(),
        inst3 in arb_compliance_instance(),
    ) {
        let instances = vec![inst1.clone(), inst2.clone(), inst3.clone()];
        let tx = build_tx_from_instances(&instances);
        let nullifiers = extract_nullifiers(&tx).expect("extract_nullifiers should succeed");

        prop_assert_eq!(nullifiers.len(), 3, "should extract 3 nullifiers");
        prop_assert_eq!(nullifiers[0], inst1.consumed_nullifier, "first nullifier should match");
        prop_assert_eq!(nullifiers[1], inst2.consumed_nullifier, "second nullifier should match");
        prop_assert_eq!(nullifiers[2], inst3.consumed_nullifier, "third nullifier should match");
    }

    /// Property: extract_commitments preserves CU order.
    #[test]
    fn prop_extract_commitments_order(
        inst1 in arb_compliance_instance(),
        inst2 in arb_compliance_instance(),
        inst3 in arb_compliance_instance(),
    ) {
        let instances = vec![inst1.clone(), inst2.clone(), inst3.clone()];
        let tx = build_tx_from_instances(&instances);
        let commitments = extract_commitments(&tx).expect("extract_commitments should succeed");

        prop_assert_eq!(commitments.len(), 3, "should extract 3 commitments");
        prop_assert_eq!(commitments[0], inst1.created_commitment, "first commitment should match");
        prop_assert_eq!(commitments[1], inst2.created_commitment, "second commitment should match");
        prop_assert_eq!(commitments[2], inst3.created_commitment, "third commitment should match");
    }

    /// Property: nullifier and commitment count matches CU count.
    #[test]
    fn prop_nullifier_commitment_count_matches_cu_count(
        instances in prop::collection::vec(arb_compliance_instance(), 1..10),
    ) {
        let tx = build_tx_from_instances(&instances);
        let nullifiers = extract_nullifiers(&tx).expect("extract_nullifiers should succeed");
        let commitments = extract_commitments(&tx).expect("extract_commitments should succeed");

        prop_assert_eq!(
            nullifiers.len(),
            instances.len(),
            "nullifier count should match instance count"
        );
        prop_assert_eq!(
            commitments.len(),
            instances.len(),
            "commitment count should match instance count"
        );
    }
}
