//! Property tests for encoding functions.

use super::strategies::{arb_aligned_bytes, arb_digest, arb_words};
use crate::encoding::{bytes_to_words, compute_action_tree_root, words_to_bytes};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: bytes→words→bytes roundtrip preserves data (for aligned input).
    #[test]
    fn prop_byte_word_roundtrip(bytes in arb_aligned_bytes(64)) {
        let words = bytes_to_words(&bytes);
        let recovered = words_to_bytes(&words);
        prop_assert_eq!(bytes, recovered, "roundtrip should preserve aligned bytes");
    }

    /// Property: aligned input recovers exactly without padding issues.
    #[test]
    fn prop_aligned_roundtrip_exact(words in arb_words(64)) {
        let bytes = words_to_bytes(&words);
        let recovered_words = bytes_to_words(&bytes);
        prop_assert_eq!(words, recovered_words, "word→byte→word should be exact");
    }

    /// Property: words_to_bytes output length equals words.len() * 4.
    #[test]
    fn prop_words_to_bytes_length(words in arb_words(64)) {
        let bytes = words_to_bytes(&words);
        prop_assert_eq!(bytes.len(), words.len() * 4, "output length should be words * 4");
    }

    /// Property: little-endian encoding is preserved.
    #[test]
    fn prop_little_endian_encoding(word in any::<u32>()) {
        let bytes = words_to_bytes(&[word]);
        let expected = word.to_le_bytes();
        prop_assert_eq!(&bytes[..], &expected[..], "should use little-endian encoding");
    }

    /// Property: action_tree_root is deterministic (same tags → same root).
    #[test]
    fn prop_action_tree_root_deterministic(
        tag1 in arb_digest(),
        tag2 in arb_digest(),
    ) {
        let tags = vec![tag1, tag2];
        let root1 = compute_action_tree_root(&tags).expect("should compute root");
        let root2 = compute_action_tree_root(&tags).expect("should compute root");
        prop_assert_eq!(root1, root2, "same tags should produce same root");
    }

    /// Property: different tags produce different roots.
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

#[test]
fn test_action_tree_root_empty_fails() {
    let result = compute_action_tree_root(&[]);
    assert!(result.is_err(), "empty tags should return error");
}
