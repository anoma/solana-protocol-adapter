//! External call encoding, decoding, and CPI execution.

use crate::encoding::{bytes_to_words, words_to_bytes};
use crate::error::PAError;
use crate::types::{ExpirableBlob, OutputMode, SolanaExternalCall};

/// Encode an external call into an ExpirableBlob.
/// Serializes using bincode and converts to word array.
pub fn encode_external_call(call: &SolanaExternalCall) -> ExpirableBlob {
    let bytes = bincode::serialize(call).expect("serialization should not fail");
    ExpirableBlob {
        blob: bytes_to_words(&bytes),
        deletion_criterion: 0,
    }
}

/// Decode an external call from an ExpirableBlob.
/// Converts from word array and deserializes using bincode.
pub fn decode_external_call(blob: &ExpirableBlob) -> Result<SolanaExternalCall, PAError> {
    let bytes = words_to_bytes(&blob.blob);
    bincode::deserialize(&bytes).map_err(|_| PAError::InvalidExternalCallBlob)
}

/// Verify that actual output matches expected output.
pub fn verify_output(expected: &[u8], actual: &[u8], _mode: &OutputMode) -> Result<(), PAError> {
    if expected.len() != actual.len() {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    if expected != actual {
        return Err(PAError::ExternalCallOutputMismatch);
    }
    Ok(())
}

// =============================================================================
// External Call Execution
// =============================================================================

/// Anchor discriminator for BlockTimeForwarder::forward_call (sha256("global:forward_call")[..8])
pub const FORWARD_CALL_DISCRIMINATOR: [u8; 8] = hex_literal::hex!("9faae00afd696cde");

/// Build instruction data for a forwarder's forward_call instruction.
/// Format: discriminator (8 bytes) + logic_ref (32 bytes) + input_len (4 bytes) + input (N bytes)
pub fn build_forwarder_instruction_data(logic_ref: &[u8; 32], input: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(8 + 32 + 4 + input.len());

    // Discriminator
    data.extend_from_slice(&FORWARD_CALL_DISCRIMINATOR);

    // logic_ref (32 bytes)
    data.extend_from_slice(logic_ref);

    // input length as u32 little-endian (Borsh format)
    data.extend_from_slice(&(input.len() as u32).to_le_bytes());

    // input data
    data.extend_from_slice(input);

    data
}

/// Execute external calls from a transaction via CPI.
///
/// For each external call in the transaction's LogicVerifierInputs:
/// 1. Build the forwarder instruction data
/// 2. Find the forwarder program in remaining_accounts
/// 3. Invoke the forwarder via CPI
/// 4. Read return data
/// 5. Verify output matches expected
///
/// # Arguments
/// * `tx` - The transaction containing external calls in LogicVerifierInputs.app_data.external_payload
/// * `remaining_accounts` - Accounts passed to the instruction, must include forwarder programs
///
/// # Returns
/// * `Ok(())` if all external calls succeed and outputs match
/// * `Err(PAError)` if any call fails or output mismatches
#[cfg(not(test))]
pub fn execute_external_calls<'info>(
    tx: &crate::types::Transaction,
    remaining_accounts: &[anchor_lang::prelude::AccountInfo<'info>],
    nullifier_count: usize,
) -> std::result::Result<(), PAError> {
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::solana_program::instruction::AccountMeta;
    use anchor_lang::solana_program::instruction::Instruction;
    use anchor_lang::solana_program::program::{get_return_data, invoke};

    use crate::settle::extract_external_calls;

    let calls = extract_external_calls(tx)?;

    if remaining_accounts.len() < nullifier_count {
        return Err(PAError::InvalidTransactionData);
    }
    let external_accounts = &remaining_accounts[nullifier_count..];

    let call_programs: Vec<Pubkey> = calls
        .iter()
        .map(|(_, call)| Pubkey::new_from_array(call.program_id))
        .collect();

    let mut cursor = 0usize;
    for (logic_ref, call) in calls {
        let program_id = Pubkey::new_from_array(call.program_id);
        let logic_ref_bytes = logic_ref.to_bytes();

        // Build instruction data
        let ix_data = build_forwarder_instruction_data(&logic_ref_bytes, &call.instruction_data);

        // remaining_accounts convention (after nullifier PDAs):
        // - For each external call, there must be a "segment" starting with the forwarder *program account*
        //   (whose key equals call.program_id), followed by any CPI accounts needed by the forwarder.
        // - Segments must appear in the same order as external calls extracted from the Transaction.
        let seg_start_rel = external_accounts[cursor..]
            .iter()
            .position(|a| a.key == &program_id)
            .ok_or(PAError::UnregisteredForwarder)?;
        let seg_start = cursor + seg_start_rel;

        let mut seg_end = external_accounts.len();
        for i in (seg_start + 1)..external_accounts.len() {
            if call_programs.iter().any(|p| external_accounts[i].key == p) {
                seg_end = i;
                break;
            }
        }

        let forwarder_program_info = &external_accounts[seg_start];
        let cpi_accounts = &external_accounts[(seg_start + 1)..seg_end];

        // Create instruction with CPI accounts metas.
        let ix_accounts: Vec<AccountMeta> = cpi_accounts
            .iter()
            .map(|ai| AccountMeta {
                pubkey: *ai.key,
                is_signer: ai.is_signer,
                is_writable: ai.is_writable,
            })
            .collect();

        let ix = Instruction {
            program_id,
            accounts: ix_accounts,
            data: ix_data,
        };

        // Invoke forwarder: pass program account + all CPI accounts.
        let mut invoke_infos: Vec<anchor_lang::prelude::AccountInfo<'info>> =
            Vec::with_capacity(1 + cpi_accounts.len());
        invoke_infos.push(forwarder_program_info.clone());
        invoke_infos.extend_from_slice(cpi_accounts);
        invoke(&ix, &invoke_infos)?;

        let actual_output = match &call.output_mode {
            OutputMode::ReturnData => {
                let (returned_program_id, return_data) =
                    get_return_data().ok_or(PAError::ExternalCallOutputMismatch)?;
                if returned_program_id != program_id {
                    return Err(PAError::ExternalCallOutputMismatch);
                }
                return_data
            }
            OutputMode::OutputAccount { index, offset, len } => {
                let idx = *index as usize;
                if idx >= remaining_accounts.len() {
                    return Err(PAError::ExternalCallOutputMismatch);
                }
                let data = remaining_accounts[idx].data.borrow();
                let start = *offset as usize;
                let end = start
                    .checked_add(*len as usize)
                    .ok_or(PAError::ExternalCallOutputMismatch)?;
                if end > data.len() {
                    return Err(PAError::ExternalCallOutputMismatch);
                }
                data[start..end].to_vec()
            }
        };

        // Verify output matches expected
        verify_output(&call.expected_output, &actual_output, &call.output_mode)?;

        // Emit ForwarderCallExecuted event (EVM parity)
        anchor_lang::prelude::emit!(crate::ForwarderCallExecutedEvent {
            forwarder: program_id,
            input: call.instruction_data.clone(),
            output: actual_output,
        });

        cursor = seg_end;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PAError;
    use crate::settle;
    use crate::test_utils::{
        create_minimal_transaction, create_transaction_with_external_payload,
        create_transaction_with_external_payload_and_logic_ref,
        create_transaction_with_multiple_lvi_external_payloads,
    };
    use crate::types::{Digest, Transaction};

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
        assert!(result.is_ok(), "Transaction with no external calls should succeed");
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
}
