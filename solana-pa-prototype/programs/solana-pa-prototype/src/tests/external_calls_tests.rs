use crate::error::PAError;
use crate::external_calls::{
    build_forwarder_instruction_data, decode_external_call, encode_external_call,
    extract_external_calls, FORWARD_CALL_DISCRIMINATOR,
};
use crate::tests::utils::{
    create_minimal_transaction, create_tag_consistent_payload_tx,
    create_transaction_with_external_payload,
};
use crate::types::{OutputMode, SolanaExternalCall};
use arm_core::logic_instance::ExpirableBlob;
use arm_core::Digest;

#[test]
fn test_extract_external_calls_empty() {
    let tx = create_minimal_transaction();
    let calls = extract_external_calls(&tx).unwrap();
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
        num_accounts: 1,
    };
    let blob = encode_external_call(&call);

    let tx = create_transaction_with_external_payload(vec![blob]);

    let extracted = extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 1, "Should extract exactly one call");

    let (_, extracted_call) = &extracted[0];
    assert_eq!(extracted_call.program_id, call.program_id);
    assert_eq!(extracted_call.instruction_data, call.instruction_data);
    assert_eq!(extracted_call.expected_output, call.expected_output);
}

/// Execution order must follow the compliance-tag traversal, not the wire order
/// of logic_verifier_inputs. Reversing the wire entries must not change the
/// order of extracted calls.
#[test]
fn test_extract_external_calls_follows_tag_order_not_wire_order() {
    let call_a = SolanaExternalCall {
        program_id: [0xAA; 32],
        instruction_data: vec![0x01],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let call_b = SolanaExternalCall {
        program_id: [0xBB; 32],
        instruction_data: vec![0x02],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };

    let tx = create_tag_consistent_payload_tx(
        vec![encode_external_call(&call_a)],
        vec![encode_external_call(&call_b)],
    );
    let ordered = extract_external_calls(&tx).expect("canonical tx must extract");
    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].1.instruction_data, vec![0x01]);
    assert_eq!(ordered[1].1.instruction_data, vec![0x02]);

    // Reverse only the wire order. The compliance units are untouched, so the
    // proof would still verify; extraction order must be unchanged.
    let mut reversed = tx.clone();
    reversed.actions[0].logic_verifier_inputs.reverse();
    let after = extract_external_calls(&reversed).expect("reordered tx must extract");

    assert_eq!(
        after.len(),
        2,
        "reordering wire entries must not drop calls"
    );
    assert_eq!(
        after[0].1.instruction_data,
        vec![0x01],
        "first call must still be the consumed-tag call"
    );
    assert_eq!(
        after[1].1.instruction_data,
        vec![0x02],
        "second call must still be the created-tag call"
    );
}

/// A tag appearing twice makes the mapping ambiguous and must be rejected.
#[test]
fn test_extract_external_calls_rejects_duplicate_tags() {
    let tx_base = create_minimal_transaction();
    let mut tx = tx_base.clone();
    tx.actions[0].logic_verifier_inputs[1].tag = tx.actions[0].logic_verifier_inputs[0].tag;

    let result = extract_external_calls(&tx);

    assert!(
        matches!(result, Err(PAError::InvalidTransactionData)),
        "duplicate LVI tags must be rejected, got {:?}",
        result
    );
}

#[test]
fn test_extract_external_calls_invalid_blob() {
    let invalid_blob = ExpirableBlob {
        blob: vec![0xDEADBEEF], // Invalid data
        deletion_criterion: 0,
    };

    let tx = create_transaction_with_external_payload(vec![invalid_blob]);

    let result = extract_external_calls(&tx);
    assert!(result.is_err(), "Invalid blob should return error");
}

#[test]
fn test_extract_external_calls_logic_ref_association() {
    let call = SolanaExternalCall {
        program_id: [0xAA; 32],
        instruction_data: vec![1, 2, 3, 4],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let blob = encode_external_call(&call);

    let verifying_key = Digest::from_bytes([0xBB; 32]);
    let mut tx = create_transaction_with_external_payload(vec![blob]);
    crate::tests::utils::mutate_cu_instance(&mut tx.actions[0].compliance_units[0], |inst| {
        inst.consumed_logic_ref = verifying_key
    });
    tx.actions[0].logic_verifier_inputs[0].verifying_key = verifying_key;

    let extracted = extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 1);

    let (logic_ref, _) = &extracted[0];
    assert_eq!(
        *logic_ref, verifying_key,
        "Logic ref should match verifying_key from LVI"
    );
}

#[test]
fn test_build_forwarder_instruction_data_byte_layout() {
    let logic_ref = [0xAA; 32];
    let input = vec![0x01, 0x02, 0x03];

    let data = build_forwarder_instruction_data(&logic_ref, &input);

    // Format: discriminator (8) + logic_ref (32) + input_len (4) + input (N)
    assert_eq!(data.len(), 8 + 32 + 4 + input.len());
    assert_eq!(&data[0..8], &FORWARD_CALL_DISCRIMINATOR, "discriminator");
    assert_eq!(&data[8..40], &logic_ref, "logic_ref");
    assert_eq!(
        &data[40..44],
        &(input.len() as u32).to_le_bytes(),
        "input length (Borsh u32 LE)"
    );
    assert_eq!(&data[44..], &input, "input payload");
}

/// Solana cannot distinguish an explicit empty return from silence, so a call
/// authorizing an empty output can never settle. Reject it at decode.
#[test]
fn test_decode_rejects_empty_expected_output() {
    let call = SolanaExternalCall {
        program_id: [7u8; 32],
        instruction_data: vec![1, 2, 3],
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let blob = encode_external_call(&call);

    let result = decode_external_call(&blob);

    assert!(
        matches!(result, Err(PAError::EmptyExpectedOutput)),
        "expected EmptyExpectedOutput, got {:?}",
        result
    );
}

/// A non-empty expected output remains valid and decodes unchanged.
#[test]
fn test_decode_accepts_non_empty_expected_output() {
    let call = SolanaExternalCall {
        program_id: [7u8; 32],
        instruction_data: vec![1, 2, 3],
        expected_output: vec![0x2a],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let blob = encode_external_call(&call);

    let decoded = decode_external_call(&blob).expect("non-empty output must decode");

    assert_eq!(decoded, call);
}
