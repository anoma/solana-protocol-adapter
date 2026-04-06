//! Tests that duplicate tags in LogicVerifierInputs are rejected by journal
//! digest computation and app_data hash verification.

use crate::encoding::{compute_batch_aggregation_journal_digest, verify_app_data_hashes};
use crate::error::PAError;
use crate::external_calls::{encode_external_call, extract_external_calls};

use crate::tests::utils::{
    create_minimal_transaction, make_external_call, make_instance_journal,
};

/// Tag count must equal LVI count. 1 CU produces 2 tags; 3 LVIs is rejected.
#[test]
fn extra_lvi_rejected_by_count_check() {
    let mut tx = create_minimal_transaction();
    // Add a third LVI with a duplicate tag.
    let duplicate_lvi = tx.actions[0].logic_verifier_inputs[0].clone();
    tx.actions[0].logic_verifier_inputs.push(duplicate_lvi);

    // 2 tags != 3 LVIs → rejected.
    let result = compute_batch_aggregation_journal_digest(&tx);
    assert!(
        matches!(result, Err(PAError::InvalidTransactionData)),
        "Extra LVI should be rejected by count check: {:?}",
        result
    );
}

/// Duplicate tag with correct count: one required tag has no matching LVI.
#[test]
fn duplicate_tag_causes_tag_not_found() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // Set both LVIs to the same tag (consumed nullifier).
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    // Also set vk to match so the vk check doesn't fail first.
    tx.actions[0].logic_verifier_inputs[1].verifying_key =
        tx.actions[0].logic_verifier_inputs[0].verifying_key;

    // Count: 2 tags == 2 LVIs → passes.
    // find_logic_input(consumed_nullifier) → LVI[0] ✓
    // find_logic_input(created_commitment) → no match → TagNotFound
    let result = compute_batch_aggregation_journal_digest(&tx);
    assert!(
        matches!(result, Err(PAError::TagNotFound)),
        "Missing tag due to duplicate should return TagNotFound: {:?}",
        result
    );
}

/// External calls are extracted from all LVIs, but journal digest computation
/// rejects the transaction before external calls can execute.
#[test]
fn duplicate_tag_external_calls_extracted_but_journal_digest_blocks() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // LVI[0]: tag=consumed_nullifier, no external calls
    // LVI[1]: tag=consumed_nullifier (duplicate), has external call
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    tx.actions[0].logic_verifier_inputs[1]
        .app_data
        .external_payload
        .push(encode_external_call(&make_external_call(
            [0xFF; 32],
            vec![1, 2, 3],
        )));

    // extract_external_calls iterates all LVIs regardless of tag uniqueness.
    let calls = extract_external_calls(&tx).unwrap();
    assert_eq!(calls.len(), 1, "Call is extracted from app_data");

    // But compute_batch_aggregation_journal_digest rejects it.
    let result = compute_batch_aggregation_journal_digest(&tx);
    assert!(
        matches!(result, Err(PAError::TagNotFound)),
        "Journal digest computation blocks before external calls can execute: {:?}",
        result
    );
}

/// verify_app_data_hashes checks all LVIs regardless of tag duplication.
#[test]
fn verify_app_data_hashes_checks_all_lvis_including_duplicates() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // Set up both LVIs with proper instance_journal containing app_data_hash.
    for lvi in &mut tx.actions[0].logic_verifier_inputs {
        lvi.instance_journal =
            make_instance_journal(lvi.tag, true, lvi.app_data.clone());
    }

    // Consistent state — should pass.
    assert!(verify_app_data_hashes(&tx).is_ok());

    // Now make LVI[1] a duplicate tag with different app_data.
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    tx.actions[0].logic_verifier_inputs[1]
        .app_data
        .external_payload
        .push(encode_external_call(&make_external_call(
            [0xAA; 32],
            vec![1, 2, 3],
        )));
    // instance_journal still has hash of OLD (empty) app_data.

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Hash check catches modified app_data on duplicate-tagged LVI: {:?}",
        result
    );
}
