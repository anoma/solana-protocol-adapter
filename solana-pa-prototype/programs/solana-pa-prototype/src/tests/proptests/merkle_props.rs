use super::strategies::arb_digest;
use crate::merkle::{current_root, hash_two, push, required_depth_for_leaves};
use crate::tests::utils::create_test_pa_state;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_hash_two_non_commutative(left in arb_digest(), right in arb_digest()) {
        prop_assume!(left != right);
        let h1 = hash_two(&left, &right);
        let h2 = hash_two(&right, &left);
        prop_assert_ne!(h1, h2, "hash_two should not be commutative");
    }

    #[test]
    fn prop_hash_two_differs_from_inputs(left in arb_digest(), right in arb_digest()) {
        let h = hash_two(&left, &right);
        prop_assert_ne!(h, left, "hash should differ from left input");
        prop_assert_ne!(h, right, "hash should differ from right input");
    }

    #[test]
    fn prop_distinct_leaves_produce_distinct_roots(
        leaf1 in arb_digest(),
        leaf2 in arb_digest(),
    ) {
        prop_assume!(leaf1 != leaf2);
        let mut state1 = create_test_pa_state();
        let mut state2 = create_test_pa_state();
        push(&mut state1, leaf1).unwrap();
        push(&mut state2, leaf2).unwrap();
        prop_assert_ne!(
            current_root(&state1),
            current_root(&state2),
            "distinct leaves should produce distinct roots"
        );
    }

    #[test]
    fn prop_append_increments_index(leaves in prop::collection::vec(arb_digest(), 1..10)) {
        let mut state = create_test_pa_state();
        for (i, leaf) in leaves.iter().enumerate() {
            prop_assert_eq!(state.next_index, i as u64, "next_index should match append count");
            push(&mut state, *leaf).unwrap();
        }
        prop_assert_eq!(state.next_index, leaves.len() as u64, "final next_index should equal leaf count");
    }

    /// With expand-after-fill, the tree at `depth` can hold up to
    /// `2^depth` leaves, but the last insertion triggers expansion to
    /// `depth+1`. So `required_depth_for_leaves(N)` returns the depth
    /// the tree will be at AFTER N appends, which is strictly greater
    /// than the capacity would need.
    #[test]
    fn prop_required_depth_sufficient_capacity(
        final_next_index in 0u64..1_000_000,
    ) {
        let depth = required_depth_for_leaves(final_next_index);
        let capacity = 1u64 << depth;
        prop_assert!(capacity >= final_next_index,
            "depth {} (capacity {}) should hold {} leaves", depth, capacity, final_next_index);
    }

    /// Verify the depth matches what `push` actually produces.
    #[test]
    fn prop_required_depth_matches_append(
        leaves in prop::collection::vec(arb_digest(), 1..32),
    ) {
        let mut state = create_test_pa_state();
        for leaf in &leaves {
            push(&mut state, *leaf).unwrap();
        }
        let expected_depth = required_depth_for_leaves(leaves.len() as u64);
        prop_assert_eq!(
            state.depth(), expected_depth,
            "required_depth_for_leaves({}) = {} but tree depth is {}",
            leaves.len(), expected_depth, state.depth()
        );
    }
}
