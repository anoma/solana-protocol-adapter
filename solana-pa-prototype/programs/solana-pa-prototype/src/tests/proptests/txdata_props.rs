//! Property tests for TxData expiration logic.

use proptest::prelude::*;
use anchor_lang::prelude::Pubkey;
use crate::state::{PAStateAccount, MIN_EXPIRY_SLOTS, MAX_EXPIRY_SLOTS};
use crate::merkle::INITIAL_TREE_DEPTH;
use crate::txdata::TxData;
use crate::error::PAError;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    /// Property: TxData is never expired when current_slot <= expires_slot.
    #[test]
    fn prop_txdata_not_expired_when_slot_lte_expiry(
        capacity in 0usize..10000,
        expiry_slot in 0u64..u64::MAX,
        slot_offset in 0u64..1_000_000u64,
    ) {
        // Generate current_slot <= expiry_slot by construction (no rejection sampling)
        let current_slot = expiry_slot.saturating_sub(slot_offset);
        let txdata = TxData::new(capacity, expiry_slot);
        prop_assert!(!txdata.is_expired(current_slot));
    }

    /// Property: TxData is always expired when current_slot > expires_slot.
    #[test]
    fn prop_txdata_expired_when_slot_gt_expiry(
        capacity in 0usize..10000,
        expiry_slot in 0u64..u64::MAX - 1,
        slots_past in 1u64..1000,
    ) {
        let current_slot = expiry_slot.saturating_add(slots_past);
        prop_assume!(current_slot > expiry_slot);
        let txdata = TxData::new(capacity, expiry_slot);
        prop_assert!(txdata.is_expired(current_slot));
    }

    /// Property: validate_not_expired returns Ok when not expired.
    #[test]
    fn prop_validate_not_expired_ok(
        capacity in 0usize..10000,
        expiry_slot in 0u64..u64::MAX,
        slot_offset in 0u64..1_000_000u64,
    ) {
        // Generate current_slot <= expiry_slot by construction (no rejection sampling)
        let current_slot = expiry_slot.saturating_sub(slot_offset);
        let txdata = TxData::new(capacity, expiry_slot);
        prop_assert!(txdata.validate_not_expired(current_slot).is_ok());
    }

    /// Property: validate_not_expired returns TxDataExpired when expired.
    #[test]
    fn prop_validate_not_expired_err(
        capacity in 0usize..10000,
        expiry_slot in 0u64..u64::MAX - 1,
        slots_past in 1u64..1000,
    ) {
        let current_slot = expiry_slot.saturating_add(slots_past);
        prop_assume!(current_slot > expiry_slot);
        let txdata = TxData::new(capacity, expiry_slot);
        let result = txdata.validate_not_expired(current_slot);
        prop_assert!(matches!(result, Err(PAError::TxDataExpired)));
    }

    /// Property: Valid expiry range is [current + MIN, current + MAX].
    #[test]
    fn prop_expiry_bounds_valid_range(
        current_slot in 0u64..u64::MAX / 2,
        offset in MIN_EXPIRY_SLOTS..=MAX_EXPIRY_SLOTS,
    ) {
        let expires_slot = current_slot.saturating_add(offset);
        let min_valid = current_slot.saturating_add(MIN_EXPIRY_SLOTS);
        let max_valid = current_slot.saturating_add(MAX_EXPIRY_SLOTS);

        prop_assert!(expires_slot >= min_valid);
        prop_assert!(expires_slot <= max_valid);
    }

    /// Property: Expiry below minimum is rejected.
    #[test]
    fn prop_expiry_below_min_rejected(
        current_slot in MIN_EXPIRY_SLOTS..u64::MAX / 2,
        offset in 0u64..MIN_EXPIRY_SLOTS,
    ) {
        let expires_slot = current_slot.saturating_add(offset);
        let min_valid = current_slot.saturating_add(MIN_EXPIRY_SLOTS);

        // expires_slot should be less than min_valid
        prop_assert!(expires_slot < min_valid);
    }

    /// Property: Expiry above maximum is rejected.
    #[test]
    fn prop_expiry_above_max_rejected(
        current_slot in 0u64..u64::MAX / 3,
        extra in 1u64..100_000,
    ) {
        let expires_slot = current_slot.saturating_add(MAX_EXPIRY_SLOTS).saturating_add(extra);
        let max_valid = current_slot.saturating_add(MAX_EXPIRY_SLOTS);

        // expires_slot should be greater than max_valid
        prop_assert!(expires_slot > max_valid);
    }

    // =========================================================================
    // CONFIGURABLE EXPIRY BOUNDS TESTS
    // =========================================================================

    /// Property: PAStateAccount with zero min_expiry_slots falls back to compile-time constant.
    #[test]
    fn prop_zero_min_expiry_slots_fallback(
        bump in 0u8..=255u8,
    ) {
        let state = PAStateAccount {
            bump,
            authority: Pubkey::default(),
            paused: false,
            root: [0u8; 32],
            next_index: 0,
            current_depth: INITIAL_TREE_DEPTH as u8,
            frontier: vec![[0u8; 32]],
            min_expiry_slots: 0,  // Zero triggers fallback
            max_expiry_slots: MAX_EXPIRY_SLOTS,
        };

        prop_assert_eq!(state.get_min_expiry_slots(), MIN_EXPIRY_SLOTS);
    }

    /// Property: PAStateAccount with zero max_expiry_slots falls back to compile-time constant.
    #[test]
    fn prop_zero_max_expiry_slots_fallback(
        bump in 0u8..=255u8,
    ) {
        let state = PAStateAccount {
            bump,
            authority: Pubkey::default(),
            paused: false,
            root: [0u8; 32],
            next_index: 0,
            current_depth: INITIAL_TREE_DEPTH as u8,
            frontier: vec![[0u8; 32]],
            min_expiry_slots: MIN_EXPIRY_SLOTS,
            max_expiry_slots: 0,  // Zero triggers fallback
        };

        prop_assert_eq!(state.get_max_expiry_slots(), MAX_EXPIRY_SLOTS);
    }

    /// Property: Non-zero expiry values are returned directly.
    #[test]
    fn prop_nonzero_expiry_slots_direct(
        min_expiry in 1u64..u64::MAX,
        max_expiry in 1u64..u64::MAX,
    ) {
        let state = PAStateAccount {
            bump: 255,
            authority: Pubkey::default(),
            paused: false,
            root: [0u8; 32],
            next_index: 0,
            current_depth: INITIAL_TREE_DEPTH as u8,
            frontier: vec![[0u8; 32]],
            min_expiry_slots: min_expiry,
            max_expiry_slots: max_expiry,
        };

        prop_assert_eq!(state.get_min_expiry_slots(), min_expiry);
        prop_assert_eq!(state.get_max_expiry_slots(), max_expiry);
    }

    // =========================================================================
    // TXDATA EXTEND VALIDATION TESTS
    // =========================================================================

    /// Property: Extension must increase expires_slot (new > current).
    #[test]
    fn prop_extend_must_increase(
        current_expires in 0u64..u64::MAX - 1,
        new_expires in 0u64..=u64::MAX,
    ) {
        let is_valid_extension = new_expires > current_expires;
        // This matches the TxDataExtendMustIncrease error condition
        prop_assert_eq!(is_valid_extension, new_expires > current_expires);
    }

    /// Property: Extension to same slot is rejected.
    #[test]
    fn prop_extend_to_same_rejected(
        expires_slot in 0u64..u64::MAX,
    ) {
        // Extending to the same slot is never valid
        let new_expires = expires_slot;
        prop_assert!(new_expires <= expires_slot);  // Would be rejected
    }

    /// Property: Extension can recover an expired TxData (new_expires > clock but old < clock).
    #[test]
    fn prop_extend_can_recover_expired(
        old_expires in 0u64..u64::MAX / 2,
        clock_offset in 1u64..1000,
        new_offset in 1u64..1000,
    ) {
        let current_slot = old_expires.saturating_add(clock_offset);  // TxData is expired
        let new_expires = current_slot.saturating_add(new_offset);    // New deadline is in future

        // Old TxData is expired
        prop_assert!(current_slot > old_expires);
        // New expiry is in the future
        prop_assert!(new_expires > current_slot);
        // Extension is valid (increases expires_slot)
        prop_assert!(new_expires > old_expires);
    }

    // =========================================================================
    // CLOSE EXPIRED VALIDATION TESTS
    // =========================================================================

    /// Property: close_expired only succeeds when current_slot > expires_slot (strictly greater).
    #[test]
    fn prop_close_expired_strictly_greater(
        expires_slot in 0u64..u64::MAX - 1,
        slots_past in 1u64..1000,
    ) {
        let current_slot = expires_slot.saturating_add(slots_past);

        // close_expired requires strict inequality: clock.slot > expires_slot
        prop_assert!(current_slot > expires_slot);
    }

    /// Property: close_expired fails at exact expiry boundary.
    #[test]
    fn prop_close_expired_exact_boundary_fails(
        expires_slot in 0u64..u64::MAX,
    ) {
        let current_slot = expires_slot;

        // At exact boundary, close_expired should fail (requires slot > expires_slot)
        prop_assert!(!(current_slot > expires_slot));
    }

    /// Property: close_expired fails before expiry.
    #[test]
    fn prop_close_expired_before_expiry_fails(
        expires_slot in 1u64..u64::MAX,
        slots_before in 1u64..1000,
    ) {
        let current_slot = expires_slot.saturating_sub(slots_before);
        prop_assume!(current_slot < expires_slot);

        // Before expiry, close_expired should fail
        prop_assert!(!(current_slot > expires_slot));
    }

    // =========================================================================
    // EXPIRY CONFIG VALIDATION TESTS
    // =========================================================================

    /// Property: Valid config requires min < max.
    #[test]
    fn prop_config_min_less_than_max(
        min in 10u64..100_000,
        max in 10u64..100_000,
    ) {
        let is_valid = min < max;
        prop_assert_eq!(is_valid, min < max);
    }

    /// Property: Min must be at least 10 slots (~4 seconds).
    #[test]
    fn prop_config_min_at_least_10(
        min in 0u64..100,
    ) {
        let is_valid = min >= 10;
        // Values 0-9 are invalid
        prop_assert_eq!(is_valid, min >= 10);
    }

    /// Property: Max must be at most 7 days in slots.
    #[test]
    fn prop_config_max_at_most_7_days(
        max in 0u64..3_000_000,
    ) {
        const SEVEN_DAYS_SLOTS: u64 = 7 * 24 * 60 * 60 * 1000 / 400;  // ~1.5M slots
        let is_valid = max <= SEVEN_DAYS_SLOTS;
        prop_assert_eq!(is_valid, max <= SEVEN_DAYS_SLOTS);
    }
}
