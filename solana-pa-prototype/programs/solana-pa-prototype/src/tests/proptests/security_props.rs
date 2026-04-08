use super::strategies::arb_digest;
use crate::encoding::{
    compute_action_tree_root, compute_batch_aggregation_journal_digest, verify_app_data_hashes,
};
use crate::error::PAError;
use crate::merkle::append_to_tree;
use crate::tests::utils::{
    build_tx_from_instances, create_minimal_transaction, create_test_pa_state,
    make_instance_journal,
};
use arm_core::compliance::ComplianceInstance;
use arm_core::logic_instance::AppData;
use arm_core::Digest;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// 1. Journal digest rejects structurally invalid transactions.
//    These test that the validation CATCHES specific corruptions,
//    not that mutation changes a hash (which is trivially true).
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Swapping two LVI tags must cause TagNotFound or a digest mismatch.
    /// This tests the tag→LVI binding in compute_batch_aggregation_journal_digest.
    #[test]
    fn swapped_lvi_tags_rejected(swap_seed in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        // Give LVIs valid instance_journals so they pass deeper checks
        let nf = tx.actions[0].compliance_units[0].instance.consumed_nullifier;
        let cm = tx.actions[0].compliance_units[0].instance.created_commitment;
        tx.actions[0].logic_verifier_inputs[0].instance_journal =
            make_instance_journal(nf, true, AppData::new());
        tx.actions[0].logic_verifier_inputs[1].instance_journal =
            make_instance_journal(cm, false, AppData::new());

        let original = compute_batch_aggregation_journal_digest(&tx);
        prop_assert!(original.is_ok(), "valid tx must produce a digest");

        // Swap the tags (nullifier ↔ commitment)
        let tag0 = tx.actions[0].logic_verifier_inputs[0].tag;
        let tag1 = tx.actions[0].logic_verifier_inputs[1].tag;
        tx.actions[0].logic_verifier_inputs[0].tag = tag1;
        tx.actions[0].logic_verifier_inputs[1].tag = tag0;

        // The verifying_key check should fail because the swapped LVI's
        // verifying_key won't match the expected logic_ref for that tag position
        let result = compute_batch_aggregation_journal_digest(&tx);
        prop_assert!(
            result.is_err() || result.unwrap() != original.unwrap(),
            "swapped LVI tags must be detected"
        );
    }

    /// Extra LVI beyond the tag count must be rejected.
    #[test]
    fn extra_lvi_rejected(extra_vk in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        let extra = arm_core::logic_instance::LogicVerifierInputs {
            tag: Digest::from_bytes(extra_vk),
            verifying_key: Digest::from_bytes(extra_vk),
            app_data: AppData::new(),
            proof: None,
            instance_journal: Vec::new(),
        };
        tx.actions[0].logic_verifier_inputs.push(extra);

        let result = compute_batch_aggregation_journal_digest(&tx);
        prop_assert!(result.is_err(), "extra LVI must be rejected: {:?}", result);
    }

    /// Removing an LVI must be rejected (fewer LVIs than tags).
    #[test]
    fn missing_lvi_rejected(_seed in 0..255u8) {
        let mut tx = create_minimal_transaction();
        tx.actions[0].logic_verifier_inputs.pop();

        let result = compute_batch_aggregation_journal_digest(&tx);
        prop_assert!(result.is_err(), "missing LVI must be rejected: {:?}", result);
    }

    /// Wrong verifying_key on an LVI must be rejected.
    #[test]
    fn wrong_verifying_key_rejected(wrong_vk in prop::array::uniform32(any::<u8>())) {
        let mut tx = create_minimal_transaction();
        let original_vk = tx.actions[0].logic_verifier_inputs[0].verifying_key;
        let new_vk = Digest::from_bytes(wrong_vk);
        prop_assume!(new_vk != original_vk);

        tx.actions[0].logic_verifier_inputs[0].verifying_key = new_vk;

        let result = compute_batch_aggregation_journal_digest(&tx);
        prop_assert!(result.is_err(), "wrong verifying_key must be rejected: {:?}", result);
    }
}

// ---------------------------------------------------------------------------
// 2. verify_app_data_hashes rejects corrupted app_data.
//    The app_data_hash in the journal must match sha256(borsh(app_data)).
//    Any mismatch must be caught.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Corrupted app_data (different from what was hashed in the journal)
    /// must be caught by verify_app_data_hashes.
    #[test]
    fn corrupted_app_data_detected(
        poison_byte in any::<u8>(),
    ) {
        let mut tx = create_minimal_transaction();
        let nf = tx.actions[0].compliance_units[0].instance.consumed_nullifier;
        let cm = tx.actions[0].compliance_units[0].instance.created_commitment;

        // Build valid journals with correct app_data_hash
        tx.actions[0].logic_verifier_inputs[0].instance_journal =
            make_instance_journal(nf, true, AppData::new());
        tx.actions[0].logic_verifier_inputs[1].instance_journal =
            make_instance_journal(cm, false, AppData::new());

        // Verify the uncorrupted tx passes
        let valid = verify_app_data_hashes(&tx);
        prop_assert!(valid.is_ok(), "valid tx must pass app_data_hash check");

        // Corrupt the app_data by adding a payload that wasn't in the hash
        tx.actions[0].logic_verifier_inputs[0]
            .app_data
            .resource_payload
            .push(arm_core::logic_instance::ExpirableBlob {
                blob: vec![poison_byte as u32],
                deletion_criterion: 0,
            });

        let result = verify_app_data_hashes(&tx);
        prop_assert!(
            matches!(result, Err(PAError::AppDataHashMismatch)),
            "corrupted app_data must be detected as AppDataHashMismatch, got: {:?}",
            result
        );
    }

    /// Truncated instance_journal (shorter than 32 bytes) must be rejected.
    #[test]
    fn truncated_journal_rejected(len in 0..31usize) {
        let mut tx = create_minimal_transaction();
        tx.actions[0].logic_verifier_inputs[0].instance_journal = vec![0u8; len];

        let result = verify_app_data_hashes(&tx);
        prop_assert!(
            result.is_err(),
            "truncated journal ({} bytes) must be rejected",
            len
        );
    }

    /// Journal with wrong alignment (not 4-byte aligned) must be rejected.
    #[test]
    fn misaligned_journal_rejected(extra_bytes in 1..3usize) {
        let mut tx = create_minimal_transaction();
        let nf = tx.actions[0].compliance_units[0].instance.consumed_nullifier;
        let mut journal = make_instance_journal(nf, true, AppData::new());
        // Add bytes to break 4-byte alignment
        for _ in 0..extra_bytes {
            journal.push(0);
        }
        tx.actions[0].logic_verifier_inputs[0].instance_journal = journal;

        let result = verify_app_data_hashes(&tx);
        prop_assert!(
            result.is_err(),
            "misaligned journal must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Merkle tree: verify against a naive reference implementation.
//    The PA's frontier-based tree must produce the same root as building
//    the full tree from scratch.
// ---------------------------------------------------------------------------

/// Naive reference Merkle tree: builds the full tree layer by layer.
fn reference_merkle_root(leaves: &[Digest]) -> Digest {
    use crate::merkle::{hash_two, PADDING_LEAF};

    if leaves.is_empty() {
        return PADDING_LEAF;
    }

    let len = leaves.len().next_power_of_two();
    let mut layer: Vec<Digest> = leaves.to_vec();
    layer.resize(len, PADDING_LEAF);

    // For the PA's tree, a full power-of-two tree has depth = log2(len) + 1
    // (the expand-after-fill semantics). After filling all slots, the tree
    // grows by one level, hashing the computed root with ZEROS[depth].
    let base_depth = len.trailing_zeros() as usize;

    let mut size = len;
    while size > 1 {
        for i in 0..size / 2 {
            layer[i] = hash_two(&layer[2 * i], &layer[2 * i + 1]);
        }
        size /= 2;
    }

    // If leaves exactly fill 2^d, the PA tree adds one more level
    if leaves.len() == len {
        use crate::merkle::ZEROS;
        hash_two(&layer[0], &ZEROS[base_depth])
    } else {
        layer[0]
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// The frontier-based append must produce the same root as the naive
    /// reference for any leaf sequence up to 16 leaves.
    #[test]
    fn merkle_tree_matches_reference(
        leaves in prop::collection::vec(arb_digest(), 1..16)
    ) {
        let mut state = create_test_pa_state();

        for leaf in &leaves {
            append_to_tree(&mut state, *leaf).unwrap();
        }

        let expected = reference_merkle_root(&leaves);
        let actual = state.root_digest();

        prop_assert_eq!(
            actual, expected,
            "frontier tree root must match reference for {} leaves",
            leaves.len()
        );
    }
}

// ---------------------------------------------------------------------------
// 4. Action tree: verify the Merkle construction catches tag substitution.
//    If an attacker replaces one tag with another, the action tree root
//    must differ. This is a collision resistance check on the tree, not
//    on the hash function.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(500))]

    /// Replacing any single tag in a 4-tag tree must change the root.
    #[test]
    fn action_tree_detects_single_tag_substitution(
        tags in prop::collection::vec(arb_digest(), 4..=4),
        replacement in arb_digest(),
        position in 0..4usize,
    ) {
        prop_assume!(tags[position] != replacement);

        let original_root = compute_action_tree_root(&tags).unwrap();

        let mut mutated = tags.clone();
        mutated[position] = replacement;

        let mutated_root = compute_action_tree_root(&mutated).unwrap();
        prop_assert_ne!(
            original_root, mutated_root,
            "tag substitution at position {} must change root",
            position
        );
    }
}
