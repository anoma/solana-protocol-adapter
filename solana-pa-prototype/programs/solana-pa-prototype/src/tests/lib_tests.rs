use crate::state::{PAStateAccount, TxDataAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
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
        // Schema version 3: the 32-byte owner after the bump, and the
        // denylist's 4-byte length after the fields.
        assert_eq!(PAStateAccount::INITIAL_SPACE, 208);
        assert_eq!(PAStateAccount::MAX_SPACE, 1200);
        assert_eq!(PAStateAccount::space(1, 0), 208);
        assert_eq!(PAStateAccount::space(2, 0), 240);
        assert_eq!(PAStateAccount::space(32, 0), 1200);
        // Each denied logic ref adds its 32 bytes.
        assert_eq!(PAStateAccount::space(1, 2), 208 + 64);
    }

    #[test]
    fn test_tx_data_account_space_calculation() {
        // discriminator(8) + bump(1) + authority(32) + refund(32) +
        // written_len(4) + expires_slot(8) + payload length prefix(4)
        assert_eq!(TxDataAccount::space(0), 89);
        assert_eq!(TxDataAccount::space(1000), 1089);
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

mod settle_order_tests {
    use crate::settle::action_resources;
    use crate::tests::utils::instance_with_consumed_and_created_payloads;

    /// The settlement visits an action's consumed resources before its
    /// created ones, as pa-evm's `_processAction` does: nullifiers, forwarder
    /// calls and payload events follow that order.
    #[test]
    fn action_resources_visits_consumed_before_created() {
        let instance = instance_with_consumed_and_created_payloads(vec![], vec![]);
        let roles: Vec<bool> = action_resources(&instance.actions[0])
            .map(|resource| resource.is_consumed)
            .collect();
        assert_eq!(roles, vec![true, false]);
    }
}
