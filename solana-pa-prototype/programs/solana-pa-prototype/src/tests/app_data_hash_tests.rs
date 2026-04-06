//! Tests for `verify_app_data_hashes`: ensures on-chain app_data matches the
//! hash committed in each LVI's `instance_journal`.

use crate::encoding::{compute_batch_aggregation_journal_digest, verify_app_data_hashes};
use crate::error::PAError;
use crate::external_calls::encode_external_call;
use arm_core::logic_instance::AppData;
use arm_core::transaction::Transaction;

use crate::tests::utils::{create_minimal_transaction, make_external_call, make_instance_journal};

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

/// The journal digest is computed over compliance instances and logic keys,
/// not over app_data. Modifications to app_data do not change the digest.
/// `verify_app_data_hashes` is the check that binds app_data to the proof.
#[test]
fn journal_digest_does_not_cover_app_data() {
    let mut tx = create_tx_with_journals(AppData::default());

    let digest_before = compute_batch_aggregation_journal_digest(&tx).unwrap();

    let call = make_external_call([0xAA; 32], vec![0xDE, 0xAD]);
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));

    let digest_after = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_eq!(
        digest_before, digest_after,
        "Journal digest does not cover app_data"
    );
}

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
    let call = make_external_call([0xBB; 32], b"extra_call".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Added external call mismatches journal hash: {:?}",
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

    // Replace with a different call
    let different_call = make_external_call([0xFF; 32], b"different".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = vec![encode_external_call(&different_call)];

    let result = verify_app_data_hashes(&tx);
    assert!(
        matches!(result, Err(PAError::AppDataHashMismatch)),
        "Replaced external call mismatches journal hash: {:?}",
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
