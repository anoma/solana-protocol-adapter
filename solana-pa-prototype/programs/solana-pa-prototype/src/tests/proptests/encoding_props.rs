use super::strategies::arb_digest;
use crate::encoding::compute_action_tree_root;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    #[test]
    fn prop_action_tree_root_sensitive_to_tags(
        tag1 in arb_digest(),
        tag2 in arb_digest(),
        tag3 in arb_digest(),
    ) {
        prop_assume!(tag2 != tag3);
        let root_a = compute_action_tree_root(&[tag1, tag2]).expect("should compute root");
        let root_b = compute_action_tree_root(&[tag1, tag3]).expect("should compute root");
        prop_assert_ne!(root_a, root_b, "different tags should produce different roots");
    }
}
