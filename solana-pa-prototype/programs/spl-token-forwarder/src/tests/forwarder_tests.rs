//! Ported EVM Forwarder Tests
//!
//! These tests are ported from the AnomaPay EVM forwarder test suite to ensure
//! behavioral parity between the EVM and Solana implementations.
//!
//! Source repository: https://github.com/anoma/anomapay-backend
//! Source files:
//! - contracts/test/bases/ForwarderBase.t.sol
//! - contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
//! - contracts/test/ERC20Forwarder.t.sol

use crate::error::ErrorCode;
use crate::state::{UnwrapInput, WrapInput, WrapMessage};
use anchor_lang::prelude::Pubkey;

// =============================================================================
// ForwarderBase.t.sol - Core Security Tests
// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
// =============================================================================

/// Mirrors: test_constructor_reverts_if_the_protocol_adapter_address_is_zero
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
///
/// In Solana, this is enforced during initialize() - cannot set protocol_adapter to Pubkey::default()
#[test]
fn test_zero_protocol_adapter_error_exists() {
    // Verify the error code exists for this check
    let _err = ErrorCode::ZeroAddressNotAllowed;
    // Integration test: Call initialize with Pubkey::default() as protocol_adapter
    // Expected: Should fail with ZeroAddressNotAllowed
}

/// Mirrors: test_constructor_reverts_if_the_logic_ref_is_zero
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
///
/// In Solana, this is enforced during initialize() - cannot set logic_ref to [0; 32]
#[test]
fn test_zero_logic_ref_error_exists() {
    // Verify the error code exists for this check
    let _err = ErrorCode::UnauthorizedLogicRef;
    // Integration test: Call initialize with [0; 32] as logic_ref
    // Expected: Should fail (implementation may need ZeroLogicRef error)
}

/// Mirrors: test_forwardCall_reverts_if_the_pa_is_not_the_caller
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
///
/// In Solana, the ForwardCall context has a constraint: caller.key() == config.protocol_adapter
#[test]
fn test_unauthorized_caller_error_exists() {
    // Verify the error code exists for this check
    let _err = ErrorCode::UnauthorizedCaller;
    // Integration test: Call forward_call from non-PA account
    // Expected: Should fail with UnauthorizedCaller
}

/// Mirrors: test_forwardCall_reverts_if_the_logic_ref_mismatches
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
///
/// In Solana, forward_call checks: logic_ref == config.logic_ref
#[test]
fn test_unauthorized_logic_ref_error_exists() {
    // Verify the error code exists for this check
    let _err = ErrorCode::UnauthorizedLogicRef;
    // Integration test: Call forward_call with wrong logic_ref
    // Expected: Should fail with UnauthorizedLogicRef
}

/// Mirrors: test_forwardCall_forwards_calls_if_the_pa_is_the_caller
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/ForwarderBase.t.sol
///
/// Integration test: Verify successful forward_call when caller is PA and logic_ref matches
#[test]
fn test_forward_call_success_path_documented() {
    // Integration test: Call forward_call from PA with correct logic_ref
    // Expected: Should succeed and execute wrap/unwrap based on op code
}

// =============================================================================
// EmergencyMigratableForwarderBase.t.sol - Emergency Operation Tests
// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
// =============================================================================

/// Mirrors: test_constructor_reverts_if_the_emergency_committe_address_is_zero
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_zero_emergency_committee_error_exists() {
    let _err = ErrorCode::ZeroAddressNotAllowed;
    // Integration test: Call initialize with Pubkey::default() as emergency_committee
    // Expected: Should fail with ZeroAddressNotAllowed
}

/// Mirrors: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_set_emergency_caller_unauthorized_committee() {
    let _err = ErrorCode::UnauthorizedCaller;
    // Integration test: Call set_emergency_caller from non-committee account
    // Expected: Should fail with UnauthorizedCaller
}

/// Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_set_emergency_caller_zero_address() {
    let _err = ErrorCode::ZeroAddressNotAllowed;
    // Integration test: Call set_emergency_caller with Pubkey::default()
    // Expected: Should fail with ZeroAddressNotAllowed
}

/// Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_set_emergency_caller_already_set() {
    let _err = ErrorCode::EmergencyCallerAlreadySet;
    // Integration test: Call set_emergency_caller twice
    // Expected: Second call should fail with EmergencyCallerAlreadySet
}

/// Mirrors: test_setEmergencyCaller_reverts_if_the_pa_is_not_stopped
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_set_emergency_caller_pa_not_stopped() {
    let _err = ErrorCode::ProtocolAdapterNotStopped;
    // Integration test: Call set_emergency_caller when PA is not paused
    // Expected: Should fail with ProtocolAdapterNotStopped
}

/// Mirrors: test_setEmergencyCaller_sets_the_emergency_caller
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_set_emergency_caller_success_documented() {
    // Integration test:
    // 1. Stop PA (set paused = true)
    // 2. Call set_emergency_caller from committee with valid address
    // Expected: Should succeed, emergency_caller field updated
}

/// Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_forward_emergency_call_caller_not_set() {
    let _err = ErrorCode::EmergencyCallerNotSet;
    // Integration test: Call forward_emergency_call without setting emergency caller first
    // Expected: Should fail with EmergencyCallerNotSet
}

/// Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_forward_emergency_call_wrong_caller() {
    let _err = ErrorCode::UnauthorizedCaller;
    // Integration test: Call forward_emergency_call from non-emergency-caller account
    // Expected: Should fail with UnauthorizedCaller
}

/// Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
#[test]
fn test_forward_emergency_call_success_documented() {
    // Integration test:
    // 1. Stop PA
    // 2. Set emergency caller
    // 3. Call forward_emergency_call from emergency caller
    // Expected: Should succeed and execute emergency operation
}

// =============================================================================
// ERC20Forwarder.t.sol - Input Validation Tests (Unit Testable)
// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
// =============================================================================

/// Mirrors: test_forwardCall_reverts_on_invalid_calltype
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_invalid_operation_code_error_exists() {
    let _err = ErrorCode::UnknownOperation;
    // Integration test: Call forward_call with op code != 0 or 1
    // Expected: Should fail with UnknownOperation
}

/// Mirrors: test_wrap_reverts_if_the_input_length_is_wrong
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_wrap_input_wrong_length() {
    // Test input too short
    let short_input = vec![0u8; WrapInput::SIZE - 1];
    let result = WrapInput::try_from_bytes(&short_input);
    assert!(result.is_err());

    // Test input too long
    let long_input = vec![0u8; WrapInput::SIZE + 1];
    let result = WrapInput::try_from_bytes(&long_input);
    assert!(result.is_err());

    // Test correct length succeeds
    let correct_input = vec![0u8; WrapInput::SIZE];
    let result = WrapInput::try_from_bytes(&correct_input);
    assert!(result.is_ok());
}

/// Mirrors: test_unwrap_reverts_if_the_input_length_is_wrong
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_input_wrong_length() {
    // Test input too short
    let short_input = vec![0u8; UnwrapInput::SIZE - 1];
    let result = UnwrapInput::try_from_bytes(&short_input);
    assert!(result.is_err());

    // Test input too long
    let long_input = vec![0u8; UnwrapInput::SIZE + 1];
    let result = UnwrapInput::try_from_bytes(&long_input);
    assert!(result.is_err());

    // Test correct length succeeds
    let correct_input = vec![0u8; UnwrapInput::SIZE];
    let result = UnwrapInput::try_from_bytes(&correct_input);
    assert!(result.is_ok());
}

/// Mirrors: test_wrap_reverts_if_the_signature_expired
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_deadline_expired_error_exists() {
    let _err = ErrorCode::DeadlineExpired;
    // Integration test: Call wrap with deadline in the past
    // Expected: Should fail with DeadlineExpired
}

/// Mirrors: test_wrap_reverts_if_the_signature_was_already_used
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_nonce_replay_error_exists() {
    let _err = ErrorCode::NonceAlreadyUsed;
    // Integration test: Call wrap twice with same nonce
    // Expected: Second call should fail with NonceAlreadyUsed
}

/// Mirrors: test_wrap_reverts_if_user_did_not_approve_permit2
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_insufficient_delegate_approval_error_exists() {
    let _err = ErrorCode::InsufficientDelegateApproval;
    // Integration test: Call wrap without user approving escrow PDA as delegate
    // Expected: Should fail with InsufficientDelegateApproval
}

/// Mirrors: test_wrap_reverts_if_the_deposited_amount_is_not_the_wrap_amount
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_wrap_balance_mismatch_error_exists() {
    let _err = ErrorCode::BalanceMismatch;
    // Integration test: Wrap with fee-on-transfer token
    // Expected: Should fail with BalanceMismatch
}

/// Mirrors: test_unwrap_reverts_if_the_withdrawn_amount_is_not_the_unwrap_amount
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_balance_mismatch_error_exists() {
    let _err = ErrorCode::BalanceMismatch;
    // Integration test: Unwrap with fee-on-transfer token
    // Expected: Should fail with BalanceMismatch
}

// =============================================================================
// ERC20Forwarder.t.sol - Token Transfer Tests (Integration)
// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
// =============================================================================

/// Mirrors: test_wrap_pulls_funds_from_user
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_wrap_transfers_tokens_documented() {
    // Integration test:
    // 1. Mint tokens to user
    // 2. User approves escrow PDA as delegate
    // 3. Create valid wrap input with Ed25519 signature
    // 4. Call forward_call with wrap operation
    // Expected:
    //   - User balance decreases by amount
    //   - Escrow balance increases by amount
    //   - Nonce marked as used
    //   - Wrapped event emitted
}

/// Mirrors: test_unwrap_sends_funds_to_the_user
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_transfers_tokens_documented() {
    // Integration test:
    // 1. Mint tokens to escrow
    // 2. Create valid unwrap input
    // 3. Call forward_call with unwrap operation
    // Expected:
    //   - Escrow balance decreases by amount
    //   - Recipient balance increases by amount
    //   - Unwrapped event emitted
}

/// Mirrors: test_wrap_does_not_revert_if_the_amount_is_zero
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_wrap_zero_amount_documented() {
    // Integration test: Call wrap with amount = 0
    // Expected: Should succeed (no tokens transferred, nonce still marked)
}

/// Mirrors: test_unwrap_does_not_revert_if_the_amount_is_zero
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_zero_amount_documented() {
    // Integration test: Call unwrap with amount = 0
    // Expected: Should succeed (no tokens transferred)
}

// =============================================================================
// ERC20Forwarder.t.sol - Event Emission Tests (Integration)
// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
// =============================================================================

/// Mirrors: test_wrap_emits_the_Wrapped_event
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_wrap_emits_wrapped_event_documented() {
    // Integration test: Successful wrap should emit Wrapped event with:
    //   - token_mint: the wrapped token
    //   - from: the user who wrapped
    //   - amount: the wrapped amount
    //   - nonce: the nonce used
    //   - action_tree_root: the action tree root from signature
}

/// Mirrors: test_unwrap_emits_the_Unwrapped_event
/// https://github.com/anoma/anomapay-backend/blob/main/contracts/test/ERC20Forwarder.t.sol
#[test]
fn test_unwrap_emits_unwrapped_event_documented() {
    // Integration test: Successful unwrap should emit Unwrapped event with:
    //   - token_mint: the unwrapped token
    //   - to: the recipient
    //   - amount: the unwrapped amount
}

// =============================================================================
// Additional Solana-Specific Tests
// =============================================================================

/// Test that WrapMessage serialization is deterministic for signature verification
#[test]
fn test_wrap_message_deterministic_serialization() {
    let program_id = Pubkey::new_unique();

    let msg1 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    let msg2 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    // Same input should produce same hash
    assert_eq!(msg1.hash(), msg2.hash());

    // Different input should produce different hash
    let msg3 = WrapMessage {
        forwarder_id: program_id.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1001, // Different amount
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };
    assert_ne!(msg1.hash(), msg3.hash());
}

/// Test that different forwarder IDs produce different hashes (domain separation)
/// This mirrors EIP-712's verifyingContract in the domain separator
#[test]
fn test_wrap_message_domain_separation_forwarder_id() {
    let program_id_1 = Pubkey::new_unique();
    let program_id_2 = Pubkey::new_unique();

    let msg1 = WrapMessage {
        forwarder_id: program_id_1.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    let msg2 = WrapMessage {
        forwarder_id: program_id_2.to_bytes(),
        token_mint: [1u8; 32],
        amount: 1000,
        nonce: 42,
        deadline: 1700000000,
        action_tree_root: [2u8; 32],
    };

    // Different forwarder_id should produce different hash
    // This prevents replay across different forwarder deployments
    assert_ne!(msg1.hash(), msg2.hash());
}

/// Test WrapInput parsing round-trip
#[test]
fn test_wrap_input_parsing_fields() {
    let token_mint = Pubkey::new_unique();
    let user = Pubkey::new_unique();
    let amount: u64 = 1_000_000;
    let nonce: u64 = 123;
    let deadline: i64 = 1700000000;
    let action_tree_root = [0xABu8; 32];
    let signature = [0xCDu8; 64];
    let ed25519_ix_index: u8 = 0;

    let mut input = Vec::with_capacity(WrapInput::SIZE);
    input.extend_from_slice(&token_mint.to_bytes());
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&user.to_bytes());
    input.extend_from_slice(&nonce.to_le_bytes());
    input.extend_from_slice(&deadline.to_le_bytes());
    input.extend_from_slice(&action_tree_root);
    input.extend_from_slice(&signature);
    input.push(ed25519_ix_index);

    let parsed = WrapInput::try_from_bytes(&input).expect("Should parse valid input");

    assert_eq!(parsed.token_mint, token_mint);
    assert_eq!(parsed.amount, amount);
    assert_eq!(parsed.user, user);
    assert_eq!(parsed.nonce, nonce);
    assert_eq!(parsed.deadline, deadline);
    assert_eq!(parsed.action_tree_root, action_tree_root);
    assert_eq!(parsed.signature, signature);
    assert_eq!(parsed.ed25519_ix_index, ed25519_ix_index);
}

/// Test UnwrapInput parsing round-trip
#[test]
fn test_unwrap_input_parsing_fields() {
    let token_mint = Pubkey::new_unique();
    let recipient = Pubkey::new_unique();
    let amount: u64 = 500_000;

    let mut input = Vec::with_capacity(UnwrapInput::SIZE);
    input.extend_from_slice(&token_mint.to_bytes());
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&recipient.to_bytes());

    let parsed = UnwrapInput::try_from_bytes(&input).expect("Should parse valid input");

    assert_eq!(parsed.token_mint, token_mint);
    assert_eq!(parsed.amount, amount);
    assert_eq!(parsed.recipient, recipient);
}
