//! Tests that duplicate tags in LogicVerifierInputs are rejected both by
//! external call extraction and by journal digest computation, so a
//! duplicate-tag transaction can never reach execution.

use crate::encoding::compute_batch_aggregation_journal_digest;
use crate::error::PAError;
use crate::external_calls::{encode_external_call, extract_external_calls};

use crate::tests::utils::{create_minimal_transaction, make_external_call};

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

/// Both external call extraction and journal digest computation reject a
/// duplicate-tag transaction. Extraction now traverses by compliance tag
/// (same order the journal uses), so a repeated tag is ambiguous there too —
/// it is no longer sufficient for journal digest computation alone to block it.
#[test]
fn duplicate_tag_rejected_by_extraction_and_journal_digest() {
    use crate::tests::utils::decode_cu_instance;

    let mut tx = create_minimal_transaction();
    let consumed_tag = decode_cu_instance(&tx.actions[0].compliance_units[0]).consumed_nullifier;

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

    // extract_external_calls now traverses by compliance tag and rejects an
    // ambiguous (repeated) tag before decoding any payload.
    let extraction_result = extract_external_calls(&tx);
    assert!(
        matches!(extraction_result, Err(PAError::InvalidTransactionData)),
        "Extraction must reject duplicate LVI tags: {:?}",
        extraction_result
    );

    // Journal digest computation rejects it too, via the same traversal. Both
    // paths now fail with InvalidTransactionData from the explicit ambiguity
    // check rather than TagNotFound, which was the downstream symptom of a
    // required tag going unmatched.
    let result = compute_batch_aggregation_journal_digest(&tx);
    assert!(
        matches!(result, Err(PAError::InvalidTransactionData)),
        "Journal digest computation must also block on the duplicate tag: {:?}",
        result
    );
}
