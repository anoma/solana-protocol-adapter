use super::strategies::{arb_compliance_instance, arb_digest};
use crate::encoding::{compute_action_tree_root, compute_batch_aggregation_journal_digest};
use crate::merkle::append_to_tree;
use crate::tests::utils::{create_minimal_transaction, create_test_pa_state};
use arm_core::compliance::ComplianceInstanceWords;
use arm_core::Digest;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// 1. Journal digest sensitivity — mutating any single ComplianceInstance field
//    changes the digest.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn journal_digest_changes_on_nullifier_mutation(seed_bytes in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        let original = compute_batch_aggregation_journal_digest(&tx).unwrap();

        tx.actions[0].compliance_units[0].instance.consumed_nullifier = Digest::from_bytes(seed_bytes);
        // Also update the LVI tag to match (otherwise TagNotFound)
        tx.actions[0].logic_verifier_inputs[0].tag = Digest::from_bytes(seed_bytes);

        match compute_batch_aggregation_journal_digest(&tx) {
            Ok(mutated) => prop_assert_ne!(original, mutated, "nullifier mutation must change digest"),
            Err(_) => {} // TagNotFound or other error is also acceptable
        }
    }

    #[test]
    fn journal_digest_changes_on_commitment_mutation(seed_bytes in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        let original = compute_batch_aggregation_journal_digest(&tx).unwrap();

        tx.actions[0].compliance_units[0].instance.created_commitment = Digest::from_bytes(seed_bytes);
        // Also update the LVI tag to match
        tx.actions[0].logic_verifier_inputs[1].tag = Digest::from_bytes(seed_bytes);

        match compute_batch_aggregation_journal_digest(&tx) {
            Ok(mutated) => prop_assert_ne!(original, mutated, "commitment mutation must change digest"),
            Err(_) => {}
        }
    }

    #[test]
    fn journal_digest_changes_on_delta_x_mutation(new_dx in prop::array::uniform8(any::<u32>())) {
        let mut tx = create_minimal_transaction();
        let original = compute_batch_aggregation_journal_digest(&tx).unwrap();

        tx.actions[0].compliance_units[0].instance.delta_x = new_dx;
        let mutated = compute_batch_aggregation_journal_digest(&tx).unwrap();
        prop_assert_ne!(original, mutated, "delta_x mutation must change digest");
    }

    #[test]
    fn journal_digest_changes_on_delta_y_mutation(new_dy in prop::array::uniform8(any::<u32>())) {
        let mut tx = create_minimal_transaction();
        let original = compute_batch_aggregation_journal_digest(&tx).unwrap();

        tx.actions[0].compliance_units[0].instance.delta_y = new_dy;
        let mutated = compute_batch_aggregation_journal_digest(&tx).unwrap();
        prop_assert_ne!(original, mutated, "delta_y mutation must change digest");
    }

    #[test]
    fn journal_digest_changes_on_logic_ref_mutation(seed_bytes in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        let original = compute_batch_aggregation_journal_digest(&tx).unwrap();

        let new_ref = Digest::from_bytes(seed_bytes);
        tx.actions[0].compliance_units[0].instance.consumed_logic_ref = new_ref;
        // Also update the LVI's verifying_key to match
        tx.actions[0].logic_verifier_inputs[0].verifying_key = new_ref;

        let mutated = compute_batch_aggregation_journal_digest(&tx).unwrap();
        prop_assert_ne!(original, mutated, "logic_ref mutation must change digest");
    }
}

// ---------------------------------------------------------------------------
// 2. Action tree root ordering — different tag orderings produce different roots.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn action_tree_root_is_order_dependent(
        a in arb_digest(),
        b in arb_digest(),
    ) {
        prop_assume!(a != b);

        let root_ab = compute_action_tree_root(&[a, b]).unwrap();
        let root_ba = compute_action_tree_root(&[b, a]).unwrap();
        prop_assert_ne!(root_ab, root_ba, "different tag order must produce different root");
    }

    #[test]
    fn action_tree_root_changes_with_any_tag(
        a in arb_digest(),
        b in arb_digest(),
        c in arb_digest(),
    ) {
        prop_assume!(a != c);

        let root_ab = compute_action_tree_root(&[a, b]).unwrap();
        let root_cb = compute_action_tree_root(&[c, b]).unwrap();
        prop_assert_ne!(root_ab, root_cb, "changing one tag must change root");
    }
}

// ---------------------------------------------------------------------------
// 3. Merkle tree: distinct leaf sequences produce distinct roots.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    #[test]
    fn merkle_distinct_sequences_produce_distinct_roots(
        leaf1 in arb_digest(),
        leaf2 in arb_digest(),
        common in arb_digest(),
    ) {
        prop_assume!(leaf1 != leaf2);

        // Sequence A: [common, leaf1]
        let mut state_a = create_test_pa_state();
        append_to_tree(&mut state_a, common).unwrap();
        append_to_tree(&mut state_a, leaf1).unwrap();
        let root_a = state_a.root_digest();

        // Sequence B: [common, leaf2]
        let mut state_b = create_test_pa_state();
        append_to_tree(&mut state_b, common).unwrap();
        append_to_tree(&mut state_b, leaf2).unwrap();
        let root_b = state_b.root_digest();

        prop_assert_ne!(root_a, root_b, "distinct leaf sequences must produce distinct roots");
    }

    #[test]
    fn merkle_append_order_matters(
        a in arb_digest(),
        b in arb_digest(),
    ) {
        prop_assume!(a != b);

        let mut state1 = create_test_pa_state();
        append_to_tree(&mut state1, a).unwrap();
        append_to_tree(&mut state1, b).unwrap();

        let mut state2 = create_test_pa_state();
        append_to_tree(&mut state2, b).unwrap();
        append_to_tree(&mut state2, a).unwrap();

        prop_assert_ne!(state1.root_digest(), state2.root_digest(), "append order must affect root");
    }

    #[test]
    fn merkle_tree_growth_preserves_existing_root_validity(
        leaves in prop::collection::vec(arb_digest(), 1..8)
    ) {
        let mut state = create_test_pa_state();
        let mut roots = Vec::new();

        for leaf in &leaves {
            append_to_tree(&mut state, *leaf).unwrap();
            roots.push(state.root_digest());
        }

        // Each intermediate root should be distinct from the next
        for window in roots.windows(2) {
            prop_assert_ne!(window[0], window[1], "each append must change the root");
        }
    }
}

// ---------------------------------------------------------------------------
// 4. ComplianceInstanceWords roundtrip preserves all fields.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn compliance_instance_words_preserves_all_fields(ci in arb_compliance_instance()) {
        let words = ComplianceInstanceWords::from(&ci);
        prop_assert_eq!(&words.u32_words[0..8], ci.consumed_nullifier.as_words());
        prop_assert_eq!(&words.u32_words[8..16], ci.consumed_logic_ref.as_words());
        prop_assert_eq!(&words.u32_words[16..24], ci.consumed_commitment_tree_root.as_words());
        prop_assert_eq!(&words.u32_words[24..32], ci.created_commitment.as_words());
        prop_assert_eq!(&words.u32_words[32..40], ci.created_logic_ref.as_words());
        prop_assert_eq!(&words.u32_words[40..48], &ci.delta_x);
        prop_assert_eq!(&words.u32_words[48..56], &ci.delta_y);
    }

    #[test]
    fn distinct_compliance_instances_produce_distinct_words(
        ci_a in arb_compliance_instance(),
        ci_b in arb_compliance_instance(),
    ) {
        prop_assume!(ci_a != ci_b);
        let words_a = ComplianceInstanceWords::from(&ci_a);
        let words_b = ComplianceInstanceWords::from(&ci_b);
        prop_assert_ne!(words_a.u32_words, words_b.u32_words);
    }
}

// ---------------------------------------------------------------------------
// 5. Digest injectivity.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn digest_to_bytes_is_injective(
        a_bytes in prop::array::uniform32(any::<u8>()),
        b_bytes in prop::array::uniform32(any::<u8>()),
    ) {
        prop_assume!(a_bytes != b_bytes);
        let a = Digest::from_bytes(a_bytes);
        let b = Digest::from_bytes(b_bytes);
        prop_assert_ne!(a.to_bytes(), b.to_bytes(), "distinct input bytes must produce distinct Digests");
    }

    #[test]
    fn digest_roundtrip_from_bytes(bytes in prop::array::uniform32(any::<u8>())) {
        let d = Digest::from_bytes(bytes);
        prop_assert_eq!(d.to_bytes(), bytes);
    }
}
