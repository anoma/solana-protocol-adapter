//! Unit tests for external_calls module.

use crate::error::PAError;
use crate::external_calls::encode_external_call;
use crate::settle;
use crate::tests::utils::{
    create_minimal_transaction, create_transaction_with_external_payload,
    create_transaction_with_external_payload_and_logic_ref,
    create_transaction_with_multiple_lvi_external_payloads,
};
use crate::types::{Digest, ExpirableBlob, OutputMode, SolanaExternalCall, Transaction};

/// Dry-run external call execution for unit testing.
/// Returns the count of external calls that would be executed.
fn execute_external_calls_dry_run(tx: &Transaction) -> Result<usize, PAError> {
    let calls = settle::extract_external_calls(tx)?;
    Ok(calls.len())
}

// =========================================================================
// EXTRACTION TESTS
// =========================================================================

#[test]
fn test_extract_external_calls_empty() {
    let tx = create_minimal_transaction();
    let calls = settle::extract_external_calls(&tx).unwrap();
    assert!(
        calls.is_empty(),
        "Transaction with no external_payload should return empty vec"
    );
}

#[test]
fn test_extract_external_calls_single() {
    let call = SolanaExternalCall {
        program_id: [0xAA; 32],
        instruction_data: vec![1, 2, 3, 4],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
    };
    let blob = encode_external_call(&call);

    let tx = create_transaction_with_external_payload(vec![blob]);

    let extracted = settle::extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 1, "Should extract exactly one call");

    let (_logic_ref, extracted_call) = &extracted[0];
    assert_eq!(extracted_call.program_id, call.program_id);
    assert_eq!(extracted_call.instruction_data, call.instruction_data);
    assert_eq!(extracted_call.expected_output, call.expected_output);
}

#[test]
fn test_extract_external_calls_multiple() {
    let call1 = SolanaExternalCall {
        program_id: [0x11; 32],
        instruction_data: vec![1, 2],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
    };
    let call2 = SolanaExternalCall {
        program_id: [0x22; 32],
        instruction_data: vec![3, 4],
        expected_output: vec![0x01],
        output_mode: OutputMode::ReturnData,
    };
    let call3 = SolanaExternalCall {
        program_id: [0x33; 32],
        instruction_data: vec![5, 6, 7, 8],
        expected_output: vec![0x02],
        output_mode: OutputMode::ReturnData,
    };

    // Two LogicVerifierInputs: first has 2 calls, second has 1 call
    let tx = create_transaction_with_multiple_lvi_external_payloads(vec![
        vec![encode_external_call(&call1), encode_external_call(&call2)],
        vec![encode_external_call(&call3)],
    ]);

    let extracted = settle::extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 3, "Should extract all 3 calls");

    assert_eq!(extracted[0].1.program_id, call1.program_id);
    assert_eq!(extracted[1].1.program_id, call2.program_id);
    assert_eq!(extracted[2].1.program_id, call3.program_id);
}

#[test]
fn test_extract_external_calls_invalid_blob() {
    let invalid_blob = ExpirableBlob {
        blob: vec![0xDEADBEEF], // Invalid data
        deletion_criterion: 0,
    };

    let tx = create_transaction_with_external_payload(vec![invalid_blob]);

    let result = settle::extract_external_calls(&tx);
    assert!(result.is_err(), "Invalid blob should return error");
}

#[test]
fn test_extract_external_calls_logic_ref_association() {
    let call = SolanaExternalCall {
        program_id: [0xAA; 32],
        instruction_data: vec![1, 2, 3, 4],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
    };
    let blob = encode_external_call(&call);

    let verifying_key = Digest::from_bytes([0xBB; 32]);
    let tx = create_transaction_with_external_payload_and_logic_ref(vec![blob], verifying_key);

    let extracted = settle::extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 1);

    let (logic_ref, _) = &extracted[0];
    assert_eq!(
        *logic_ref, verifying_key,
        "Logic ref should match verifying_key from LVI"
    );
}

// =========================================================================
// EXECUTION TESTS
// =========================================================================

#[test]
fn test_execute_external_calls_no_calls() {
    let tx = create_minimal_transaction();
    let result = execute_external_calls_dry_run(&tx);
    assert!(
        result.is_ok(),
        "Transaction with no external calls should succeed"
    );
    assert_eq!(result.unwrap(), 0, "Should return 0 calls executed");
}

#[test]
fn test_execute_external_calls_count() {
    let call = SolanaExternalCall {
        program_id: [0xAA; 32],
        instruction_data: vec![1, 2, 3, 4, 5, 6, 7, 8],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
    };
    let blob = encode_external_call(&call);
    let tx = create_transaction_with_external_payload(vec![blob]);

    let result = execute_external_calls_dry_run(&tx);
    assert!(result.is_ok(), "Should succeed in dry run");
    assert_eq!(result.unwrap(), 1, "Should return 1 call to execute");
}

#[test]
fn test_execute_external_calls_multiple_count() {
    let call1 = SolanaExternalCall {
        program_id: [0x11; 32],
        instruction_data: vec![1, 2],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
    };
    let call2 = SolanaExternalCall {
        program_id: [0x22; 32],
        instruction_data: vec![3, 4],
        expected_output: vec![0x01],
        output_mode: OutputMode::ReturnData,
    };

    let tx = create_transaction_with_multiple_lvi_external_payloads(vec![
        vec![encode_external_call(&call1)],
        vec![encode_external_call(&call2)],
    ]);

    let result = execute_external_calls_dry_run(&tx);
    assert!(result.is_ok(), "Should succeed in dry run");
    assert_eq!(result.unwrap(), 2, "Should return 2 calls to execute");
}
