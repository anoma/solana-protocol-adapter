use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::tests::utils::create_minimal_transaction;
use arm_core::transaction::Transaction;
use arm_core::Digest;

mod fixture_tests {
    use super::*;

    #[test]
    fn test_digest_bincode_layout() {
        let digest = Digest::from_bytes([
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e,
            0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c,
            0x1d, 0x1e, 0x1f, 0x20,
        ]);
        let serialized = bincode::serialize(&digest).unwrap();
        assert_eq!(serialized.len(), 32);

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

mod governance_tests {
    use super::*;

    #[test]
    fn test_pa_state_account_space_calculation() {
        assert_eq!(PAStateAccount::INITIAL_SPACE, 236);
        assert_eq!(PAStateAccount::MAX_SPACE, 1228);
        assert_eq!(PAStateAccount::space_for_depth(1), 236);
        assert_eq!(PAStateAccount::space_for_depth(2), 268);
        assert_eq!(PAStateAccount::space_for_depth(32), 1228);
    }
}

mod txdata_expiry_bounds_tests {
    use super::*;

    #[test]
    fn test_expiry_bounds_no_overflow() {
        let current_slot: u64 = u64::MAX - 50;
        let min_expires = current_slot.saturating_add(MIN_EXPIRY_SLOTS);
        let max_expires = current_slot.saturating_add(MAX_EXPIRY_SLOTS);

        assert_eq!(min_expires, u64::MAX);
        assert_eq!(max_expires, u64::MAX);
    }
}
