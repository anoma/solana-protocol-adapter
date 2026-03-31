//! H-002: Duplicate logic tag investigation.
//!
//! Tests whether an attacker can exploit duplicate tags in LogicVerifierInputs
//! to inject unverified external calls.

use crate::encoding::{compute_batch_aggregation_journal_digest, verify_app_data_hashes};
use crate::error::PAError;
use crate::external_calls::{encode_external_call, extract_external_calls};
use crate::types::{OutputMode, SolanaExternalCall};
use arm_core::logic_instance::LogicInstance;
use arm_core::Digest;

use crate::tests::utils::create_minimal_transaction;

fn make_external_call(program_id: [u8; 32]) -> SolanaExternalCall {
    SolanaExternalCall {
        program_id,
        instruction_data: vec![1, 2, 3],
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
    }
}

/// Count check blocks extra LVIs: 1 CU produces 2 tags, 3 LVIs rejected.
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

/// Duplicate tag with correct count: one tag is missing → TagNotFound.
/// Attacker provides 2 LVIs both with tag=consumed_nullifier.
/// The created_commitment tag has no matching LVI.
#[test]
fn duplicate_tag_causes_tag_not_found() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // Set both LVIs to the same tag (consumed nullifier).
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    // Also set vk to match so the vk check doesn't fail first.
    tx.actions[0].logic_verifier_inputs[1].verifying_key = tx.actions[0].logic_verifier_inputs[0].verifying_key;

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

/// Even if extract_external_calls iterates all LVIs, the journal digest
/// computation rejects the transaction before external calls execute.
/// This confirms the duplicate-tag attack cannot reach CPI execution.
#[test]
fn duplicate_tag_external_calls_extracted_but_journal_digest_blocks() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // LVI[0]: honest, tag=consumed_nullifier, no external calls
    // LVI[1]: malicious, tag=consumed_nullifier (duplicate), has external call
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    tx.actions[0].logic_verifier_inputs[1]
        .app_data
        .external_payload
        .push(encode_external_call(&make_external_call([0xFF; 32])));

    // extract_external_calls sees the malicious call (it iterates all LVIs).
    let calls = extract_external_calls(&tx).unwrap();
    assert_eq!(calls.len(), 1, "Malicious call IS extracted from app_data");

    // But compute_batch_aggregation_journal_digest rejects it.
    let result = compute_batch_aggregation_journal_digest(&tx);
    assert!(
        matches!(result, Err(PAError::TagNotFound)),
        "Journal digest computation blocks before external calls can execute: {:?}",
        result
    );
}

/// verify_app_data_hashes checks ALL LVIs regardless of tag duplication.
/// Even if an attacker could somehow bypass the journal digest check,
/// the hash check would catch mismatched app_data.
#[test]
fn verify_app_data_hashes_checks_all_lvis_including_duplicates() {
    let mut tx = create_minimal_transaction();
    let consumed_tag = tx.actions[0].compliance_units[0]
        .instance
        .consumed_nullifier;

    // Set up both LVIs with proper instance_journal containing app_data_hash.
    for lvi in &mut tx.actions[0].logic_verifier_inputs {
        let mut instance = LogicInstance {
            tag: lvi.tag,
            is_consumed: true,
            root: Digest::default(),
            app_data: lvi.app_data.clone(),
            app_data_hash: Digest::default(),
        };
        instance.compute_and_set_app_data_hash();
        lvi.instance_journal = instance.to_journal().unwrap();
    }

    // Consistent state — should pass.
    assert!(verify_app_data_hashes(&tx).is_ok());

    // Now make LVI[1] a duplicate tag with different app_data.
    tx.actions[0].logic_verifier_inputs[1].tag = consumed_tag;
    tx.actions[0].logic_verifier_inputs[1]
        .app_data
        .external_payload
        .push(encode_external_call(&make_external_call([0xAA; 32])));
    // instance_journal still has hash of OLD (empty) app_data.

    // verify_app_data_hashes catches the mismatch on LVI[1].
    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Hash check catches tampered duplicate LVI: {:?}",
        result
    );
}
