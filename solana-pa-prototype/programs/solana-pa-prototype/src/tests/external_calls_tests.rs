use crate::error::PAError;
use crate::external_calls::{
    build_account_metas, build_forwarder_instruction_data, decode_external_call,
    encode_external_call, extract_external_calls, FORWARD_CALL_DISCRIMINATOR,
};
use crate::tests::utils::{
    instance_with_consumed_and_created_payloads, instance_with_external_payload, make_account_info,
    minimal_instance,
};
use crate::types::{OutputMode, SolanaExternalCall};
use anchor_lang::prelude::Pubkey;
use arm_core::logic_instance::ExpirableBlob;
use arm_core::Digest;

#[test]
fn test_extract_external_calls_empty() {
    let instance = minimal_instance();
    let calls = extract_external_calls(&instance).unwrap();
    assert!(
        calls.is_empty(),
        "Instance with no external_payload should return empty vec"
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

    let instance = instance_with_external_payload(vec![blob]);

    let extracted = extract_external_calls(&instance).unwrap();
    assert_eq!(extracted.len(), 1, "Should extract exactly one call");

    let (_, extracted_call) = &extracted[0];
    assert_eq!(extracted_call.program_id, call.program_id);
    assert_eq!(extracted_call.instruction_data, call.instruction_data);
    assert_eq!(extracted_call.expected_output, call.expected_output);
}

/// Execution order is the instance order the proof commits to: consumed
/// resources before created resources within each action.
#[test]
fn test_extract_external_calls_consumed_before_created() {
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

    let instance = instance_with_consumed_and_created_payloads(
        vec![encode_external_call(&call_a)],
        vec![encode_external_call(&call_b)],
    );
    let ordered = extract_external_calls(&instance).expect("instance must extract");
    assert_eq!(ordered.len(), 2);
    assert_eq!(
        ordered[0].1.instruction_data,
        vec![0x01],
        "first call must be the consumed resource's call"
    );
    assert_eq!(
        ordered[1].1.instruction_data,
        vec![0x02],
        "second call must be the created resource's call"
    );
}

#[test]
fn test_extract_external_calls_invalid_blob() {
    let invalid_blob = ExpirableBlob {
        blob: vec![0xDEADBEEF], // Invalid data
        deletion_criterion: 0,
    };

    let instance = instance_with_external_payload(vec![invalid_blob]);

    let result = extract_external_calls(&instance);
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

    let logic_ref = Digest::from_bytes([0xBB; 32]);
    let mut instance = instance_with_external_payload(vec![blob]);
    instance.actions[0].consumed_publics[0].resource_logic_ref = logic_ref;

    let extracted = extract_external_calls(&instance).unwrap();
    assert_eq!(extracted.len(), 1);

    let (extracted_logic_ref, _) = &extracted[0];
    assert_eq!(
        *extracted_logic_ref, logic_ref,
        "Logic ref should match the resource's logic ref"
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

/// Signer authority must never reach a forwarder. Solana unions privileges
/// across the message, so an outer signer can appear in a forwarded position
/// without that position requesting it.
#[test]
fn test_build_account_metas_never_propagates_signer() {
    let program_id = Pubkey::new_unique();
    let forwarder_key = Pubkey::new_unique();
    let payer_key = Pubkey::new_unique();

    make_account_info!(forwarder, &forwarder_key, owner: &program_id,
        lamports: 0, signer: false, writable: false, executable: true);
    // The outer payer: signer and writable by virtue of the outer message.
    make_account_info!(payer, &payer_key, owner: &program_id,
        lamports: 1_000_000, signer: true, writable: true, executable: false);

    let segment = [forwarder.clone(), payer.clone()];
    let metas = build_account_metas(&segment);

    assert_eq!(metas.len(), 1, "metas cover segment[1..], not the program");
    assert_eq!(metas[0].pubkey, payer_key);
    assert!(
        !metas[0].is_signer,
        "signer authority must never be forwarded to proof-selected code"
    );
    assert!(
        metas[0].is_writable,
        "writability is preserved; only signer authority is withheld"
    );
}
