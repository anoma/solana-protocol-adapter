//! Property tests for delta accumulation and verification.

use proptest::prelude::*;
use crate::delta::{collect_tags, compute_verifying_key, accumulate_deltas, verify_delta_proof};
use crate::types::Delta;
use super::strategies::{arb_compliance_instance, arb_compliance_instance_zero_delta, build_tx_from_instances};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: collect_tags returns tags in [nf1, cm1, nf2, cm2, ...] order.
    #[test]
    fn prop_collect_tags_order(
        inst1 in arb_compliance_instance(),
        inst2 in arb_compliance_instance(),
    ) {
        let instances = vec![inst1.clone(), inst2.clone()];
        let tx = build_tx_from_instances(&instances);
        let tags = collect_tags(&tx).unwrap();

        // Expected order: nf1, cm1, nf2, cm2
        prop_assert_eq!(tags.len(), 4, "should have 4 tags for 2 instances");
        prop_assert_eq!(tags[0], inst1.consumed_nullifier.to_bytes(), "first tag should be inst1 nullifier");
        prop_assert_eq!(tags[1], inst1.created_commitment.to_bytes(), "second tag should be inst1 commitment");
        prop_assert_eq!(tags[2], inst2.consumed_nullifier.to_bytes(), "third tag should be inst2 nullifier");
        prop_assert_eq!(tags[3], inst2.created_commitment.to_bytes(), "fourth tag should be inst2 commitment");
    }

    /// Property: compute_verifying_key is deterministic (same tags → same VK).
    #[test]
    fn prop_verifying_key_deterministic(
        inst in arb_compliance_instance(),
    ) {
        let instances = vec![inst.clone()];
        let tx = build_tx_from_instances(&instances);
        let tags = collect_tags(&tx).unwrap();

        let vk1 = compute_verifying_key(&tags);
        let vk2 = compute_verifying_key(&tags);
        prop_assert_eq!(vk1, vk2, "same tags should produce same verifying key");
    }

    /// Property: zero deltas accumulate to identity (None).
    #[test]
    fn prop_zero_deltas_accumulate_to_identity(
        inst1 in arb_compliance_instance_zero_delta(),
        inst2 in arb_compliance_instance_zero_delta(),
    ) {
        let instances = vec![inst1, inst2];
        let tx = build_tx_from_instances(&instances);
        let result = accumulate_deltas(&tx).unwrap();
        prop_assert!(result.is_none(), "zero deltas should accumulate to identity (None)");
    }

    /// Property: witness delta_proof is rejected by verify_delta_proof.
    #[test]
    fn prop_witness_rejected(inst in arb_compliance_instance_zero_delta()) {
        let mut tx = build_tx_from_instances(&[inst]);
        tx.delta_proof = Delta::Witness(vec![1, 2, 3]);

        let result = verify_delta_proof(&tx);
        prop_assert!(result.is_err(), "witness delta_proof should be rejected");
    }
}
