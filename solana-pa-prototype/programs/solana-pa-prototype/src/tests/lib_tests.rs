//! Unit tests for lib module (main program tests).

use crate::external_calls::{
    build_forwarder_instruction_data, encode_external_call, FORWARD_CALL_DISCRIMINATOR,
};
use crate::settle;
use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::tests::utils::*;
use crate::types::*;

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
    fn test_transaction_bincode_roundtrip() {
        let tx = create_minimal_transaction();
        let serialized = bincode::serialize(&tx).unwrap();
        let deserialized: Transaction = bincode::deserialize(&serialized).unwrap();

        assert_eq!(tx, deserialized);
    }
}

// =========================================================================
// EXTERNAL CALL STRUCTURE TESTS
// =========================================================================

mod integration_tests {
    use super::*;

    #[test]
    fn test_external_call_transaction_structure() {
        use block_time_forwarder::RESULT_LT;

        let forwarder_program_id = [0x11; 32];
        let expected_time: i64 = 0; // Unix epoch - far in past
        let call = SolanaExternalCall {
            program_id: forwarder_program_id,
            instruction_data: expected_time.to_le_bytes().to_vec(),
            expected_output: vec![RESULT_LT],
            output_mode: OutputMode::ReturnData,
        };

        let blob = encode_external_call(&call);
        let tx = create_transaction_with_external_payload(vec![blob]);

        let extracted = settle::extract_external_calls(&tx).unwrap();
        assert_eq!(extracted.len(), 1, "Should have 1 external call");

        let (logic_ref, extracted_call) = &extracted[0];
        assert_eq!(extracted_call.program_id, forwarder_program_id);
        assert_eq!(
            extracted_call.instruction_data,
            expected_time.to_le_bytes().to_vec()
        );
        assert_eq!(extracted_call.expected_output, vec![RESULT_LT]);

        let logic_ref_bytes = logic_ref.to_bytes();
        let ix_data =
            build_forwarder_instruction_data(&logic_ref_bytes, &extracted_call.instruction_data);

        // discriminator (8) + logic_ref (32) + len (4) + data (8) = 52 bytes
        assert_eq!(ix_data.len(), 52);
        assert_eq!(&ix_data[0..8], &FORWARD_CALL_DISCRIMINATOR);
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

}

// =========================================================================
// TXDATA EXPIRY BOUNDS TESTS
// =========================================================================

mod txdata_expiry_bounds_tests {
    use super::*;

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
