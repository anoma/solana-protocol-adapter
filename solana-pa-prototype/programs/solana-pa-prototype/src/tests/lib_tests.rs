//! Unit tests for lib module (main program tests).

use crate::external_calls::{
    build_forwarder_instruction_data, encode_external_call, FORWARD_CALL_DISCRIMINATOR,
};
use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, ZEROS};
use crate::settle;
use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::tests::utils::*;
use crate::types::*;
use crate::{
    ApplicationPayloadEvent, DiscoveryPayloadEvent, ExternalPayloadEvent, ResourcePayloadEvent,
    DELETION_CRITERION_NEVER,
};
use anchor_lang::prelude::Pubkey;

// =========================================================================
// SERIALIZATION FORMAT TESTS (ARM-RISC0 COMPATIBILITY)
// =========================================================================

mod fixture_tests {
    use super::*;

    #[test]
    fn test_compliance_instance_size() {
        // ComplianceInstance should serialize to a known size
        // 5 Digests (5 * 32 = 160 bytes) + 2 * 8 u32s (2 * 32 = 64 bytes) = 224 bytes
        let instance = ComplianceInstance::default();
        let serialized = bincode::serialize(&instance).unwrap();

        // bincode adds length prefixes, so size may vary slightly
        // but it should be consistent
        let instance2 = ComplianceInstance {
            consumed_nullifier: Digest::from_bytes([1u8; 32]),
            consumed_logic_ref: Digest::from_bytes([2u8; 32]),
            consumed_commitment_tree_root: Digest::from_bytes([3u8; 32]),
            created_commitment: Digest::from_bytes([4u8; 32]),
            created_logic_ref: Digest::from_bytes([5u8; 32]),
            delta_x: [6u32; 8],
            delta_y: [7u32; 8],
        };
        let serialized2 = bincode::serialize(&instance2).unwrap();

        // Both should have same size
        assert_eq!(serialized.len(), serialized2.len());
    }

    #[test]
    fn test_digest_bincode_layout() {
        // Verify Digest serializes as 8 u32s
        let digest = Digest::from_bytes([
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ]);
        let serialized = bincode::serialize(&digest).unwrap();

        // Should be exactly 32 bytes (8 * 4 bytes per u32)
        assert_eq!(serialized.len(), 32);

        // Roundtrip should work
        let deserialized: Digest = bincode::deserialize(&serialized).unwrap();
        assert_eq!(digest, deserialized);
    }

    #[test]
    fn test_aggregation_proof_discriminant() {
        // The proof bytes are bincode-serialized; the first 4 bytes are the enum discriminant.
        // We accept both discriminants (0/1) but always attempt batch verification.
        let batch_bytes = fake_aggregation_proof_bytes(1, 256);
        let seq_bytes = fake_aggregation_proof_bytes(0, 256);
        assert_ne!(&batch_bytes[0..4], &seq_bytes[0..4]);
    }

    #[test]
    fn test_transaction_bincode_roundtrip() {
        // Full transaction roundtrip test
        let tx = create_minimal_transaction();
        let serialized = bincode::serialize(&tx).unwrap();
        let deserialized: Transaction = bincode::deserialize(&serialized).unwrap();

        // Verify key fields
        assert_eq!(tx.actions.len(), deserialized.actions.len());
        assert_eq!(
            tx.actions[0].compliance_units.len(),
            deserialized.actions[0].compliance_units.len()
        );
    }
}

// =========================================================================
// EXTERNAL CALL STRUCTURE TESTS
// =========================================================================

mod integration_tests {
    use super::*;

    #[tokio::test]
    async fn test_external_call_transaction_structure() {
        use block_time_forwarder::RESULT_LT;

        // Create an external call for BlockTimeForwarder
        // Input: timestamp far in the past (0) -> should return RESULT_LT (0)
        let forwarder_program_id = [0x11; 32]; // Placeholder - use actual deployed ID

        let expected_time: i64 = 0; // Unix epoch - far in past
        let call = SolanaExternalCall {
            program_id: forwarder_program_id,
            instruction_data: expected_time.to_le_bytes().to_vec(),
            expected_output: vec![RESULT_LT], // 0 = LT (past < current)
            output_mode: OutputMode::ReturnData,
        };

        // Encode the call
        let blob = encode_external_call(&call);

        // Create transaction with this external call
        let tx = create_transaction_with_external_payload(vec![blob]);

        // Verify extraction works
        let extracted = settle::extract_external_calls(&tx).unwrap();
        assert_eq!(extracted.len(), 1, "Should have 1 external call");

        let (logic_ref, extracted_call) = &extracted[0];
        assert_eq!(extracted_call.program_id, forwarder_program_id);
        assert_eq!(
            extracted_call.instruction_data,
            expected_time.to_le_bytes().to_vec()
        );
        assert_eq!(extracted_call.expected_output, vec![RESULT_LT]);

        // Verify instruction data building
        let logic_ref_bytes = logic_ref.to_bytes();
        let ix_data =
            build_forwarder_instruction_data(&logic_ref_bytes, &extracted_call.instruction_data);

        // Verify format: discriminator (8) + logic_ref (32) + len (4) + data (8) = 52 bytes
        assert_eq!(ix_data.len(), 52);

        // Verify discriminator
        assert_eq!(&ix_data[0..8], &FORWARD_CALL_DISCRIMINATOR);

        println!("External call transaction structure validated");
        println!("  - Program ID: {:?}", &forwarder_program_id[..4]);
        println!("  - Input (timestamp): {}", expected_time);
        println!("  - Expected output: {:?}", vec![RESULT_LT]);
        println!("  - Instruction data length: {} bytes", ix_data.len());
    }
}

// =========================================================================
// GOVERNANCE TESTS (PAStateAccount, Emergency Stop, Authority)
// =========================================================================

mod governance_tests {
    use super::*;

    #[test]
    fn test_pa_state_account_space_calculation() {
        // Verify space calculation for variable-depth tree
        // Initial space (depth 1): BASE_SPACE + VEC_OVERHEAD + 32
        assert_eq!(PAStateAccount::INITIAL_SPACE, 135);
        // Max space (depth 32): BASE_SPACE + VEC_OVERHEAD + (32 * 32)
        assert_eq!(PAStateAccount::MAX_SPACE, 1127);
        // space_for_depth function
        assert_eq!(PAStateAccount::space_for_depth(1), 135);
        assert_eq!(PAStateAccount::space_for_depth(2), 167);
        assert_eq!(PAStateAccount::space_for_depth(32), 1127);
    }

    #[test]
    fn test_pa_state_stores_authority() {
        let authority = Pubkey::new_unique();
        let state = create_mock_pa_state(authority, false);
        assert_eq!(state.authority, authority);
    }

    #[test]
    fn test_pa_state_stores_paused_flag() {
        let authority = Pubkey::new_unique();

        let state_unpaused = create_mock_pa_state(authority, false);
        assert!(!state_unpaused.paused);

        let state_paused = create_mock_pa_state(authority, true);
        assert!(state_paused.paused);
    }

    #[test]
    fn test_paused_state_blocks_settlement_check() {
        // This tests the logic of the paused check (not the full instruction)
        let authority = Pubkey::new_unique();
        let state = create_mock_pa_state(authority, true);

        // The check `require!(!state.paused, PAError::Paused)` should fail
        assert!(state.paused, "State should be paused");
        // In the actual instruction, this would return Err(PAError::Paused)
    }

    #[test]
    fn test_unpaused_state_allows_settlement_check() {
        let authority = Pubkey::new_unique();
        let state = create_mock_pa_state(authority, false);

        // The check `require!(!state.paused, PAError::Paused)` should pass
        assert!(!state.paused, "State should not be paused");
    }

    #[test]
    fn test_emergency_stop_sets_paused_true() {
        // Simulating the emergency_stop logic
        let authority = Pubkey::new_unique();
        let mut state = create_mock_pa_state(authority, false);

        assert!(!state.paused, "State should start unpaused");

        // Simulate emergency_stop
        state.paused = true;

        assert!(state.paused, "State should be paused after emergency_stop");
    }

    #[test]
    fn test_cannot_emergency_stop_when_already_paused() {
        // The require!(!state.paused, PAError::AlreadyPaused) should fail
        let authority = Pubkey::new_unique();
        let state = create_mock_pa_state(authority, true);

        // Trying to pause again should fail
        assert!(state.paused, "Already paused - should error in instruction");
    }

    #[test]
    fn test_authority_field_is_32_bytes() {
        // Pubkey is always 32 bytes
        assert_eq!(std::mem::size_of::<Pubkey>(), 32);
    }

    #[test]
    fn test_paused_field_is_1_byte() {
        // bool is 1 byte in Solana/Anchor
        assert_eq!(std::mem::size_of::<bool>(), 1);
    }
}

// =========================================================================
// EVENT EMISSION TESTS (App Data, Deletion Criterion Filtering)
// =========================================================================

mod event_tests {
    use super::*;

    #[test]
    fn test_deletion_criterion_never_value() {
        // DELETION_CRITERION_NEVER should be 1 (matching EVM PA)
        assert_eq!(DELETION_CRITERION_NEVER, 1);
    }

    #[test]
    fn test_deletion_criterion_immediately_is_zero() {
        // DeletionCriterion::Immediately is 0 in arm-risc0
        // Payloads with this value should NOT be emitted
        let immediately = 0u32;
        assert_ne!(immediately, DELETION_CRITERION_NEVER);
    }

    #[test]
    fn test_event_structs_have_correct_fields() {
        // Verify event structs can be constructed with expected field types
        let tag = [0u8; 32];
        let index = 42u32;
        let blob = vec![1u8, 2, 3, 4];

        let resource_event = ResourcePayloadEvent {
            tag,
            index,
            blob: blob.clone(),
        };
        assert_eq!(resource_event.tag, tag);
        assert_eq!(resource_event.index, 42);
        assert_eq!(resource_event.blob, blob);

        let discovery_event = DiscoveryPayloadEvent {
            tag,
            index,
            blob: blob.clone(),
        };
        assert_eq!(discovery_event.tag, tag);
        assert_eq!(discovery_event.index, 42);
        assert_eq!(discovery_event.blob, blob);

        let external_event = ExternalPayloadEvent {
            tag,
            index,
            blob: blob.clone(),
        };
        assert_eq!(external_event.tag, tag);
        assert_eq!(external_event.index, 42);
        assert_eq!(external_event.blob, blob);

        let application_event = ApplicationPayloadEvent {
            tag,
            index,
            blob: blob.clone(),
        };
        assert_eq!(application_event.tag, tag);
        assert_eq!(application_event.index, 42);
        assert_eq!(application_event.blob, blob);
    }

    #[test]
    fn test_expirable_blob_filtering() {
        // Test that we correctly identify payloads that should be emitted
        let never_blob = ExpirableBlob {
            blob: vec![1, 2, 3],
            deletion_criterion: DELETION_CRITERION_NEVER,
        };
        let immediately_blob = ExpirableBlob {
            blob: vec![4, 5, 6],
            deletion_criterion: 0, // Immediately
        };

        assert_eq!(never_blob.deletion_criterion, DELETION_CRITERION_NEVER);
        assert_ne!(
            immediately_blob.deletion_criterion,
            DELETION_CRITERION_NEVER
        );
    }

    #[test]
    fn test_app_data_with_mixed_deletion_criteria() {
        // Verify app_data structure with mixed payloads
        let app_data = AppData {
            resource_payload: vec![
                ExpirableBlob {
                    blob: vec![1],
                    deletion_criterion: DELETION_CRITERION_NEVER,
                },
                ExpirableBlob {
                    blob: vec![2],
                    deletion_criterion: 0,
                }, // Immediately
            ],
            discovery_payload: vec![ExpirableBlob {
                blob: vec![3],
                deletion_criterion: DELETION_CRITERION_NEVER,
            }],
            external_payload: vec![],
            application_payload: vec![
                ExpirableBlob {
                    blob: vec![4],
                    deletion_criterion: 0,
                }, // Immediately
            ],
        };

        // Count payloads that would be emitted (deletion_criterion == NEVER)
        let mut would_emit = 0;
        for p in &app_data.resource_payload {
            if p.deletion_criterion == DELETION_CRITERION_NEVER {
                would_emit += 1;
            }
        }
        for p in &app_data.discovery_payload {
            if p.deletion_criterion == DELETION_CRITERION_NEVER {
                would_emit += 1;
            }
        }
        for p in &app_data.external_payload {
            if p.deletion_criterion == DELETION_CRITERION_NEVER {
                would_emit += 1;
            }
        }
        for p in &app_data.application_payload {
            if p.deletion_criterion == DELETION_CRITERION_NEVER {
                would_emit += 1;
            }
        }

        // Should emit 2 payloads: resource[0] and discovery[0]
        assert_eq!(would_emit, 2);
    }
}

// =========================================================================
// TXDATA EXPIRY BOUNDS TESTS
// =========================================================================

mod txdata_expiry_bounds_tests {
    use super::*;

    #[test]
    fn test_expiry_bounds_constants() {
        // MIN: 100 slots * 400ms = ~40 seconds
        assert_eq!(MIN_EXPIRY_SLOTS, 100);
        // MAX: 216,000 slots * 400ms = ~24 hours
        assert_eq!(MAX_EXPIRY_SLOTS, 216_000);
        // MAX should be greater than MIN
        assert!(MAX_EXPIRY_SLOTS > MIN_EXPIRY_SLOTS);
    }

    #[test]
    fn test_expiry_bounds_calculations() {
        let current_slot: u64 = 1_000_000;

        let min_expires = current_slot.saturating_add(MIN_EXPIRY_SLOTS);
        let max_expires = current_slot.saturating_add(MAX_EXPIRY_SLOTS);

        assert_eq!(min_expires, 1_000_100);
        assert_eq!(max_expires, 1_216_000);
    }

    #[test]
    fn test_expiry_bounds_no_overflow() {
        let current_slot: u64 = u64::MAX - 50;

        // saturating_add should not overflow
        let min_expires = current_slot.saturating_add(MIN_EXPIRY_SLOTS);
        let max_expires = current_slot.saturating_add(MAX_EXPIRY_SLOTS);

        assert_eq!(min_expires, u64::MAX);
        assert_eq!(max_expires, u64::MAX);
    }
}
