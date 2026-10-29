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
        // Schema version 4: the 32-byte owner after the bump, and the two
        // denylists' 4-byte lengths after the fields.
        assert_eq!(PAStateAccount::INITIAL_SPACE, 180);
        assert_eq!(PAStateAccount::MAX_SPACE, 1236);
        assert_eq!(PAStateAccount::space(1, 0), 212);
        assert_eq!(PAStateAccount::space(2, 0), 244);
        assert_eq!(PAStateAccount::space(32, 0), 1204);
        // The tree of 2^32 leaves, the deepest, is 33 levels deep.
        assert_eq!(PAStateAccount::space(33, 0), 1236);
        // Each denylist entry adds its 32 bytes.
        assert_eq!(PAStateAccount::space(1, 2), 212 + 64);
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
    use crate::error::PAError;
    use crate::settle::process_action;
    use crate::tests::utils::{consumed, created};
    use crate::ActionExecuted;
    use arm_core::aggregation_instance::{ActionAggregated, AggregationResourceInstance};
    use arm_core::Digest;

    /// An action whose list interleaves the kinds: created, consumed, created,
    /// consumed.
    fn interleaved_action() -> ActionAggregated {
        ActionAggregated {
            resource_instances: vec![created(1), consumed(2), created(3), consumed(4)],
            delta_x: [0u32; 8],
            delta_y: [0u32; 8],
            action_tree_root: Digest::from_bytes([5u8; 32]),
        }
    }

    /// The settlement takes an action's resources in the order of its list,
    /// as pa-evm's `_processAction`: each resource's checks, nullifier or
    /// commitment, forwarder calls and payload events run when
    /// `process_action` visits it, so they follow that order, and
    /// ActionExecuted holds each kind in list order.
    #[test]
    fn process_action_visits_the_resources_in_list_order_as_pa_evm() {
        let action = interleaved_action();

        let mut visited = Vec::new();
        let executed = process_action(&action, |resource| {
            visited.push((resource.is_consumed(), resource.tag()));
            Ok(())
        })
        .unwrap();

        let tag = |byte: u8| Digest::from_bytes([byte; 32]);
        assert_eq!(
            visited,
            vec![
                (false, tag(1)),
                (true, tag(2)),
                (false, tag(3)),
                (true, tag(4))
            ],
            "resources visited, as (consumed, tag), in list order"
        );
        let expected = ActionExecuted {
            action_tree_root: [5u8; 32],
            nullifiers: vec![[2u8; 32], [4u8; 32]],
            consumed_logic_refs: vec![[102u8; 32], [104u8; 32]],
            commitments: vec![[1u8; 32], [3u8; 32]],
            created_logic_refs: vec![[101u8; 32], [103u8; 32]],
        };
        assert_eq!(
            borsh::to_vec(&executed).unwrap(),
            borsh::to_vec(&expected).unwrap(),
            "ActionExecuted: {executed:?}, expected {expected:?}"
        );
    }

    /// A resource that fails its checks aborts the action where it stands, as
    /// pa-evm's revert inside the loop: the resources before it were
    /// processed, none after it is.
    #[test]
    fn process_action_stops_at_the_first_failing_resource() {
        let action = interleaved_action();
        let failing = action.resource_instances[2].tag();

        let mut visited = Vec::new();
        let result = process_action(&action, |resource| {
            visited.push(resource.tag());
            if resource.tag() == failing {
                return Err(PAError::NonExistingRoot.into());
            }
            Ok(())
        });

        assert!(result.is_err(), "the failing resource fails the action");
        assert_eq!(
            visited,
            action.resource_instances[..3]
                .iter()
                .map(AggregationResourceInstance::tag)
                .collect::<Vec<_>>(),
            "the resources up to the failing one are visited, in list order"
        );
    }
}
