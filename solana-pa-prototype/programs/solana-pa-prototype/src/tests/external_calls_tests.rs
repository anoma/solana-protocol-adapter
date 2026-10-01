use crate::error::PAError;
use crate::external_calls::{
    build_account_metas, build_forwarder_instruction_data, decode_external_call,
    decode_external_calls, encode_external_call, FORWARD_CALL_DISCRIMINATOR,
};
use crate::tests::utils::make_account_info;
use crate::types::{OutputMode, SolanaExternalCall};
use anchor_lang::prelude::Pubkey;
use arm_core::logic_instance::{AppData, ExpirableBlob};

fn call(program_id: u8, instruction_data: Vec<u8>) -> SolanaExternalCall {
    SolanaExternalCall {
        program_id: [program_id; 32],
        instruction_data,
        expected_output: vec![0x00],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    }
}

#[test]
fn decode_external_calls_of_app_data_without_calls_is_empty() {
    let calls = decode_external_calls(&AppData::default()).unwrap();
    assert!(
        calls.is_empty(),
        "app data with no external payload has no calls"
    );
}

/// A resource's calls run in the order its app data lists them, as pa-evm's
/// `_executeForwarderCalls` iterates the external payload.
#[test]
fn decode_external_calls_keeps_the_listed_order() {
    let (first, second) = (call(0xAA, vec![1]), call(0xBB, vec![2]));
    let app_data = AppData {
        external_payload: vec![encode_external_call(&first), encode_external_call(&second)],
        ..AppData::default()
    };
    let decoded = decode_external_calls(&app_data).unwrap();
    assert_eq!(decoded.len(), 2);
    assert_eq!(decoded[0].program_id, first.program_id);
    assert_eq!(decoded[0].instruction_data, first.instruction_data);
    assert_eq!(decoded[1].program_id, second.program_id);
    assert_eq!(decoded[1].instruction_data, second.instruction_data);
}

#[test]
fn decode_external_calls_rejects_an_invalid_blob() {
    let app_data = AppData {
        external_payload: vec![ExpirableBlob {
            blob: vec![0xDEADBEEF],
            deletion_criterion: 0,
        }],
        ..AppData::default()
    };
    assert!(decode_external_calls(&app_data).is_err());
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
