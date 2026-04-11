//! H-001 regression: mutating `lvi.app_data` must change the output of
//! `compute_batch_aggregation_journal_digest`.
//!
//! Under the re-derivation defense, the PA reconstructs each LVI's journal
//! bytes from the structured `app_data` at verification time. Any attacker
//! mutation — injecting, replacing, or stripping external calls — feeds into
//! the aggregation digest and causes Groth16 verification to fail downstream.

use crate::encoding::compute_batch_aggregation_journal_digest;
use crate::external_calls::encode_external_call;

use crate::tests::utils::{create_minimal_transaction, make_external_call};

#[test]
fn injecting_external_call_changes_digest() {
    let mut tx = create_minimal_transaction();
    let digest_before = compute_batch_aggregation_journal_digest(&tx).unwrap();

    let call = make_external_call([0xAA; 32], vec![0xDE, 0xAD]);
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));

    let digest_after = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_ne!(
        digest_before, digest_after,
        "Injected external call must change the re-derived journal digest"
    );
}

#[test]
fn replacing_external_call_changes_digest() {
    let mut tx = create_minimal_transaction();
    let legitimate = make_external_call([0x11; 32], b"legitimate".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&legitimate));
    let digest_honest = compute_batch_aggregation_journal_digest(&tx).unwrap();

    let malicious = make_external_call([0xFF; 32], b"drain_escrow".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = vec![encode_external_call(&malicious)];

    let digest_tampered = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_ne!(
        digest_honest, digest_tampered,
        "Replaced external call must change the re-derived journal digest"
    );
}

#[test]
fn stripping_external_call_changes_digest() {
    let mut tx = create_minimal_transaction();
    let call = make_external_call([0x11; 32], b"block_time".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&call));
    let digest_with_call = compute_batch_aggregation_journal_digest(&tx).unwrap();

    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .clear();

    let digest_stripped = compute_batch_aggregation_journal_digest(&tx).unwrap();
    assert_ne!(
        digest_with_call, digest_stripped,
        "Stripped external call must change the re-derived journal digest"
    );
}
