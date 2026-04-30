use crate::external_calls::{
    build_forwarder_instruction_data, encode_external_call, extract_external_calls,
    FORWARD_CALL_DISCRIMINATOR,
};
use crate::tests::utils::{
    create_minimal_transaction, create_transaction_with_external_payload,
    create_transaction_with_multi_lvi_payloads,
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

#[test]
fn test_extract_external_calls_multiple() {
    let call1 = SolanaExternalCall {
        program_id: [0x11; 32],
        instruction_data: vec![1, 2],
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let call2 = SolanaExternalCall {
        program_id: [0x22; 32],
        instruction_data: vec![3, 4],
        expected_output: vec![0x01],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };
    let call3 = SolanaExternalCall {
        program_id: [0x33; 32],
        instruction_data: vec![5, 6, 7, 8],
        expected_output: vec![0x02],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    };

    // Two LogicVerifierInputs: first has 2 calls, second has 1 call
    let tx = create_transaction_with_multi_lvi_payloads(vec![
        vec![encode_external_call(&call1), encode_external_call(&call2)],
        vec![encode_external_call(&call3)],
    ]);

    let extracted = extract_external_calls(&tx).unwrap();
    assert_eq!(extracted.len(), 3, "Should extract all 3 calls");

    let (_, c0) = &extracted[0];
    let (_, c1) = &extracted[1];
    let (_, c2) = &extracted[2];
    assert_eq!(*c0, call1, "call 0 mismatch");
    assert_eq!(*c1, call2, "call 1 mismatch");
    assert_eq!(*c2, call3, "call 2 mismatch");
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
    {
        // Wire format: instance is journal bytes — parse, mutate, re-encode.
        use arm_core::compliance::ComplianceInstance;
        let cu = &mut tx.actions[0].compliance_units[0];
        let mut instance = ComplianceInstance::from_journal(&cu.instance)
            .expect("test fixture compliance instance should decode");
        instance.consumed_logic_ref = verifying_key;
        cu.instance = instance
            .to_journal()
            .expect("mutated compliance instance should re-encode");
    }
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
