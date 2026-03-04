use crate::external_calls::encode_external_call;
use crate::settle;
use crate::tests::utils::{
    create_minimal_transaction, create_transaction_with_external_payload,
    create_transaction_with_external_payload_and_logic_ref,
    create_transaction_with_multiple_lvi_external_payloads,
};
use crate::types::{Digest, ExpirableBlob, OutputMode, SolanaExternalCall};

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
