//! Shared test fixtures and helpers for unit tests.
//!
//! This module provides common test utilities used across multiple test modules.

#![allow(dead_code)] // Test utilities may be used in future tests
#![allow(unused_imports)] // Keep imports for future test utilities

use anchor_lang::prelude::{AnchorSerialize, Pubkey};

use crate::groth16::{Proof, Seal, Selector};
use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, ZEROS};
use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::types::*;

/// Arbitrary selector for unit tests. Used by `fake_aggregation_proof_bytes` to test
/// that the proof parser correctly extracts the selector from verifier_parameters.
/// This value is not meaningful - actual selectors are extracted from real proofs.
pub const FAKE_SELECTOR: Selector = [0x31, 0x0f, 0xe5, 0x98];

/// Generate fake aggregation proof bytes for testing.
pub fn fake_aggregation_proof_bytes() -> Vec<u8> {
    let proof = Proof {
        pi_a: [0u8; 64],
        pi_b: [1u8; 128],
        pi_c: [2u8; 64],
    };
    let seal = Seal {
        selector: FAKE_SELECTOR,
        proof,
    };

    seal.try_to_vec().unwrap()
}

/// Create a minimal transaction for testing.
///
/// Contains one action with one compliance unit and two LogicVerifierInputs
/// (consumed and created resources).
pub fn create_minimal_transaction() -> Transaction {
    let empty_tree_root = EMPTY_TREE_ROOT_INITIAL;
    let consumed_nullifier = Digest::from_bytes([1u8; 32]);
    let created_commitment = Digest::from_bytes([2u8; 32]);
    let consumed_logic_ref = Digest::from_bytes([3u8; 32]);
    let created_logic_ref = Digest::from_bytes([4u8; 32]);

    let instance = ComplianceInstance {
        consumed_nullifier,
        consumed_logic_ref,
        consumed_commitment_tree_root: empty_tree_root,
        created_commitment,
        created_logic_ref,
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };
    Transaction {
        actions: vec![Action {
            compliance_units: vec![ComplianceUnit {
                instance,
                proof: None,
            }],
            logic_verifier_inputs: vec![
                LogicVerifierInputs {
                    tag: consumed_nullifier,
                    verifying_key: consumed_logic_ref,
                    app_data: AppData::default(),
                    proof: None,
                },
                LogicVerifierInputs {
                    tag: created_commitment,
                    verifying_key: created_logic_ref,
                    app_data: AppData::default(),
                    proof: None,
                },
            ],
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
}

/// Create a compliance instance with specified nullifier and commitment.
pub fn create_compliance_instance(nullifier: Digest, commitment: Digest) -> ComplianceInstance {
    let empty_tree_root = EMPTY_TREE_ROOT_INITIAL;
    ComplianceInstance {
        consumed_nullifier: nullifier,
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: empty_tree_root,
        created_commitment: commitment,
        created_logic_ref: Digest::default(),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    }
}

/// Create a transaction with the given compliance instances.
pub fn create_transaction_with_compliance_instances(
    instances: Vec<ComplianceInstance>,
) -> Transaction {
    // Build logic_verifier_inputs first (before consuming instances)
    let mut logic_verifier_inputs: Vec<LogicVerifierInputs> = Vec::new();
    for inst in &instances {
        logic_verifier_inputs.push(LogicVerifierInputs {
            tag: inst.consumed_nullifier,
            verifying_key: inst.consumed_logic_ref,
            app_data: AppData::default(),
            proof: None,
        });
        logic_verifier_inputs.push(LogicVerifierInputs {
            tag: inst.created_commitment,
            verifying_key: inst.created_logic_ref,
            app_data: AppData::default(),
            proof: None,
        });
    }

    let compliance_units: Vec<ComplianceUnit> = instances
        .into_iter()
        .map(|instance| ComplianceUnit {
            instance,
            proof: None,
        })
        .collect();

    Transaction {
        actions: vec![Action {
            compliance_units,
            logic_verifier_inputs,
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
}

/// Create a transaction with external_payload in a single LogicVerifierInputs.
pub fn create_transaction_with_external_payload(payloads: Vec<ExpirableBlob>) -> Transaction {
    let mut tx = create_minimal_transaction();
    // Put payloads on the consumed-resource LVI (tag = consumed_nullifier).
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = payloads;
    tx
}

/// Create a transaction with external_payload and a specific verifying_key (logic_ref).
pub fn create_transaction_with_external_payload_and_logic_ref(
    payloads: Vec<ExpirableBlob>,
    verifying_key: Digest,
) -> Transaction {
    let mut tx = create_minimal_transaction();

    // Update the consumed logic ref in the compliance instance, and align the LVI's verifying_key.
    let cu = &mut tx.actions[0].compliance_units[0];
    cu.instance.consumed_logic_ref = verifying_key;

    tx.actions[0].logic_verifier_inputs[0].verifying_key = verifying_key;
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = payloads;

    tx
}

/// Create a transaction with multiple LogicVerifierInputs, each with their own external_payloads.
pub fn create_transaction_with_multiple_lvi_external_payloads(
    payloads_per_lvi: Vec<Vec<ExpirableBlob>>,
) -> Transaction {
    let empty_tree_root = EMPTY_TREE_ROOT_INITIAL;
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::default(),
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: empty_tree_root,
        created_commitment: Digest::default(),
        created_logic_ref: Digest::default(),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };

    let logic_verifier_inputs: Vec<LogicVerifierInputs> = payloads_per_lvi
        .into_iter()
        .enumerate()
        .map(|(i, payloads)| LogicVerifierInputs {
            tag: Digest::default(),
            verifying_key: Digest::from_bytes([i as u8; 32]), // Different verifying_key for each
            app_data: AppData {
                resource_payload: vec![],
                discovery_payload: vec![],
                external_payload: payloads,
                application_payload: vec![],
            },
            proof: None,
        })
        .collect();

    Transaction {
        actions: vec![Action {
            compliance_units: vec![ComplianceUnit {
                instance,
                proof: None,
            }],
            logic_verifier_inputs,
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
}

/// Create a fresh PAStateAccount for testing merkle operations.
/// Uses variable-depth tree starting at depth 1 (capacity = 2 leaves).
pub fn create_test_pa_state() -> PAStateAccount {
    PAStateAccount {
        bump: 0,
        authority: Pubkey::default(),
        paused: false,
        root: ZEROS[INITIAL_TREE_DEPTH - 1].to_bytes(), // ZEROS[0] = PADDING_LEAF for depth 1
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![ZEROS[0].to_bytes()], // Single entry for depth 1
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    }
}

/// Create a PAStateAccount at a specific depth for testing.
pub fn create_test_pa_state_at_depth(depth: u8) -> PAStateAccount {
    PAStateAccount {
        bump: 0,
        authority: Pubkey::default(),
        paused: false,
        root: ZEROS[depth as usize - 1].to_bytes(),
        next_index: 0,
        current_depth: depth,
        frontier: vec![[0u8; 32]; depth as usize],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    }
}

/// Create a mock PAStateAccount for testing with specified authority and paused state.
pub fn create_mock_pa_state(authority: Pubkey, paused: bool) -> PAStateAccount {
    PAStateAccount {
        bump: 255,
        authority,
        paused,
        root: EMPTY_TREE_ROOT_INITIAL.to_bytes(),
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![ZEROS[0].to_bytes()],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    }
}
