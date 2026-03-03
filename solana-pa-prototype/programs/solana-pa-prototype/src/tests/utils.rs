//! Shared test fixtures and helpers for unit tests.
//!
//! This module provides common test utilities used across multiple test modules.

use anchor_lang::prelude::{AnchorSerialize, Pubkey};

use crate::groth16::Seal;
use groth_16_verifier::Proof;
use verifier_router::Selector;
use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, ZEROS};
use crate::state::{PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use crate::types::*;

/// Helper to create a Transaction from a slice of ComplianceInstances.
pub fn build_tx_from_instances(instances: &[ComplianceInstance]) -> Transaction {
    let cus: Vec<ComplianceUnit> = instances
        .iter()
        .map(|inst| ComplianceUnit {
            instance: inst.clone(),
            proof: None,
        })
        .collect();

    let mut lvis = Vec::new();
    for inst in instances {
        lvis.push(LogicVerifierInputs {
            tag: inst.consumed_nullifier,
            verifying_key: inst.consumed_logic_ref,
            app_data: AppData::default(),
            proof: None,
            instance_journal: Vec::new(),
        });
        lvis.push(LogicVerifierInputs {
            tag: inst.created_commitment,
            verifying_key: inst.created_logic_ref,
            app_data: AppData::default(),
            proof: None,
            instance_journal: Vec::new(),
        });
    }

    Transaction {
        actions: vec![Action {
            compliance_units: cus,
            logic_verifier_inputs: lvis,
        }],
        delta_proof: Delta::Witness(DeltaWitness([0u8; 32])),
        expected_balance: None,
        aggregation_proof: None,
    }
}

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
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::from_bytes([1u8; 32]),
        consumed_logic_ref: Digest::from_bytes([3u8; 32]),
        consumed_commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
        created_commitment: Digest::from_bytes([2u8; 32]),
        created_logic_ref: Digest::from_bytes([4u8; 32]),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
    };
    build_tx_from_instances(&[instance])
}

/// Create a compliance instance with specified nullifier and commitment.
pub fn create_compliance_instance(nullifier: Digest, commitment: Digest) -> ComplianceInstance {
    ComplianceInstance {
        consumed_nullifier: nullifier,
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
        created_commitment: commitment,
        created_logic_ref: Digest::default(),
        delta_x: [0u32; 8],
        delta_y: [0u32; 8],
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
    let instance = ComplianceInstance {
        consumed_nullifier: Digest::default(),
        consumed_logic_ref: Digest::default(),
        consumed_commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
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
            verifying_key: Digest::from_bytes([i as u8; 32]),
            app_data: AppData {
                external_payload: payloads,
                ..AppData::default()
            },
            proof: None,
            instance_journal: Vec::new(),
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
        delta_proof: Delta::Witness(DeltaWitness([0u8; 32])),
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
