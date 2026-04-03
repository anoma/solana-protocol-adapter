//! Tests for H-001: app_data substitution attack and its fix.
//!
//! The first three tests demonstrate that the journal digest is structurally
//! blind to app_data changes (the underlying vulnerability).
//!
//! The last three tests verify that `verify_app_data_hashes` catches the
//! substitution by checking the hash committed in `instance_journal`.

use crate::encoding::{compute_batch_aggregation_journal_digest, verify_app_data_hashes};
use crate::error::PAError;
use crate::external_calls::encode_external_call;
use arm_core::logic_instance::{AppData, LogicInstance};
use arm_core::transaction::Transaction;
use arm_core::Digest;

use crate::tests::utils::{create_minimal_transaction, make_external_call};

fn make_instance_journal(tag: Digest, is_consumed: bool, app_data: AppData) -> Vec<u8> {
    let mut instance = LogicInstance {
        tag,
        is_consumed,
        root: Digest::default(),
        app_data,
        app_data_hash: Digest::default(),
    };
    instance.compute_and_set_app_data_hash();
    instance
        .to_journal()
        .expect("borsh serialization should succeed")
}

fn create_tx_with_journals(consumed_app_data: AppData) -> Transaction {
    let mut tx = create_minimal_transaction();
    let action = &mut tx.actions[0];
    let consumed_tag = action.compliance_units[0].instance.consumed_nullifier;
    let created_tag = action.compliance_units[0].instance.created_commitment;
    action.logic_verifier_inputs[0].instance_journal =
        make_instance_journal(consumed_tag, true, consumed_app_data.clone());
    action.logic_verifier_inputs[1].instance_journal =
        make_instance_journal(created_tag, false, AppData::default());
    action.logic_verifier_inputs[0].app_data = consumed_app_data;
    tx
}

// ============================================================================
// Vulnerability demonstration: journal digest is blind to app_data
// ============================================================================

#[test]
fn journal_digest_is_blind_to_app_data_changes() {
    let mut tx = create_tx_with_journals(AppData::default());

    let digest_before = compute_batch_aggregation_journal_digest(&tx).unwrap();

    // Inject a call into app_data — journal digest should NOT change.
    let call = make_external_call([0xAA; 32], vec![0xDE, 0xAD]);
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));

    let digest_after = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_eq!(
        digest_before, digest_after,
        "Journal digest is blind to app_data — this is the structural vulnerability"
    );
}

// ============================================================================
// Fix verification: verify_app_data_hashes catches substitution
// ============================================================================

#[test]
fn verify_app_data_hashes_passes_for_consistent_data() {
    let tx = create_tx_with_journals(AppData::default());
    // app_data is default (empty) — matches what was hashed in the journal

    assert!(
        verify_app_data_hashes(&tx).is_ok(),
        "Consistent app_data and instance_journal should pass"
    );
}

#[test]
fn verify_app_data_hashes_rejects_injected_external_call() {
    let mut tx = create_tx_with_journals(AppData::default());

    // Inject a call into app_data — this doesn't match the hash in the journal
    let call = make_external_call([0xBB; 32], b"steal_tokens".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Injected external call should be caught by hash check: {:?}",
        result
    );
}

#[test]
fn verify_app_data_hashes_rejects_replaced_external_call() {
    // Journal was computed with a legitimate external call
    let legitimate_call = make_external_call([0x11; 32], b"legitimate".to_vec());
    let mut honest_app_data = AppData::default();
    honest_app_data
        .external_payload
        .push(encode_external_call(&legitimate_call));

    let mut tx = create_tx_with_journals(honest_app_data);

    // Consistent state — should pass
    assert!(verify_app_data_hashes(&tx).is_ok());

    // Now replace with a malicious call
    let malicious_call = make_external_call([0xFF; 32], b"drain_escrow".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = vec![encode_external_call(&malicious_call)];

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Replaced external call should be caught by hash check: {:?}",
        result
    );
}

#[test]
fn verify_app_data_hashes_rejects_stripped_external_call() {
    // Journal was computed WITH an external call
    let call = make_external_call([0x11; 32], b"block_time".to_vec());
    let mut app_data = AppData::default();
    app_data.external_payload.push(encode_external_call(&call));

    let mut tx = create_tx_with_journals(app_data);
    // Strip the call from app_data — journal still has hash of the version WITH the call
    tx.actions[0].logic_verifier_inputs[0].app_data = AppData::default();

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Stripped external call should be caught by hash check: {:?}",
        result
    );
}
