//! Property tests for Merkle tree operations.

use proptest::prelude::*;
use crate::merkle::hash_two;
use crate::proptests::strategies::arb_digest;
use crate::test_utils::create_test_pa_state;
use crate::{append_to_tree, compute_root_from_frontier};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: hash_two is deterministic (same inputs → same output).
    #[test]
    fn prop_hash_two_deterministic(left in arb_digest(), right in arb_digest()) {
        let h1 = hash_two(&left, &right);
        let h2 = hash_two(&left, &right);
        prop_assert_eq!(h1, h2, "hash_two should be deterministic");
    }

    /// Property: hash_two is non-commutative (hash(a,b) ≠ hash(b,a) for a ≠ b).
    #[test]
    fn prop_hash_two_non_commutative(left in arb_digest(), right in arb_digest()) {
        prop_assume!(left != right);
        let h1 = hash_two(&left, &right);
        let h2 = hash_two(&right, &left);
        prop_assert_ne!(h1, h2, "hash_two should not be commutative");
    }

    /// Property: hash_two output differs from both inputs.
    #[test]
    fn prop_hash_two_differs_from_inputs(left in arb_digest(), right in arb_digest()) {
        let h = hash_two(&left, &right);
        prop_assert_ne!(h, left, "hash should differ from left input");
        prop_assert_ne!(h, right, "hash should differ from right input");
    }

    /// Property: distinct leaves produce distinct roots.
    #[test]
    fn prop_distinct_leaves_produce_distinct_roots(
        leaf1 in arb_digest(),
        leaf2 in arb_digest(),
    ) {
        prop_assume!(leaf1 != leaf2);
        let mut state1 = create_test_pa_state();
        let mut state2 = create_test_pa_state();
        append_to_tree(&mut state1, leaf1);
        append_to_tree(&mut state2, leaf2);
        prop_assert_ne!(
            compute_root_from_frontier(&state1),
            compute_root_from_frontier(&state2),
            "distinct leaves should produce distinct roots"
        );
    }

    /// Property: append increments next_index correctly.
    #[test]
    fn prop_append_increments_index(leaves in prop::collection::vec(arb_digest(), 1..10)) {
        let mut state = create_test_pa_state();
        for (i, leaf) in leaves.iter().enumerate() {
            prop_assert_eq!(state.next_index, i as u64, "next_index should match append count");
            append_to_tree(&mut state, *leaf);
        }
        prop_assert_eq!(state.next_index, leaves.len() as u64, "final next_index should equal leaf count");
    }
}
