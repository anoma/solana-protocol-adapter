use super::strategies::arb_compliance_instance;
use crate::settle::{extract_commitments, extract_nullifiers};
use crate::tests::utils::build_tx_from_instances;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_extract_preserves_count_and_order(
        instances in prop::collection::vec(arb_compliance_instance(), 1..10),
    ) {
        let tx = build_tx_from_instances(&instances);
        let nullifiers = extract_nullifiers(&tx).expect("extract_nullifiers should not fail on test instances");
        let commitments = extract_commitments(&tx).expect("extract_commitments should not fail on test instances");

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

        for (i, inst) in instances.iter().enumerate() {
            prop_assert_eq!(
                nullifiers[i], inst.consumed_nullifier,
                "nullifier at index {} should match", i
            );
            prop_assert_eq!(
                commitments[i], inst.created_commitment,
                "commitment at index {} should match", i
            );
        }
    }
}
