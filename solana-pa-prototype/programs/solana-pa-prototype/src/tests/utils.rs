use anchor_lang::prelude::{AnchorSerialize, Pubkey};

/// Declare an `AccountInfo` with owned backing storage via name-shadowing.
///
/// The first binding creates a `(u64, Vec<u8>)` tuple that owns lamports and data.
/// The second binding shadows it with the `AccountInfo` that borrows from the tuple.
/// Shadowing keeps the original tuple alive for the scope's duration.
macro_rules! make_account_info {
    ($name:ident, $key:expr, owner: $owner:expr, lamports: $lamports:expr,
     signer: $signer:expr, writable: $writable:expr, executable: $executable:expr) => {
        #[allow(unused_mut)]
        let mut $name = ($lamports as u64, Vec::<u8>::new());
        let $name = ::anchor_lang::prelude::AccountInfo::new(
            $key,
            $signer,
            $writable,
            &mut $name.0,
            &mut $name.1,
            $owner,
            $executable,
            0,
        );
    };
}
pub(crate) use make_account_info;

/// Assert that a Result is an AnchorError whose `error_name` matches the given PAError variant.
///
/// Anchor wraps `#[error_code]` variants into `AnchorError { error_name, error_code_number, .. }`.
/// This helper extracts the error name and compares it to avoid stringly-typed checks at call sites.
macro_rules! assert_anchor_err {
    ($result:expr, $variant:ident) => {{
        let err = $result.expect_err(concat!("Expected PAError::", stringify!($variant)));
        match err {
            ::anchor_lang::error::Error::AnchorError(ref e) => {
                assert_eq!(
                    e.error_name,
                    stringify!($variant),
                    "Expected PAError::{}, got PAError::{}",
                    stringify!($variant),
                    e.error_name,
                );
            }
            _ => panic!(
                "Expected AnchorError(PAError::{}), got non-Anchor error: {:?}",
                stringify!($variant),
                err,
            ),
        }
    }};
}
pub(crate) use assert_anchor_err;

use crate::merkle::{EMPTY_TREE_ROOT_INITIAL, INITIAL_TREE_DEPTH, ZEROS};
use crate::state::{PALifecycle, PAStateAccount, MAX_EXPIRY_SLOTS, MIN_EXPIRY_SLOTS};
use arm_core::action::Action;
use arm_core::compliance::ComplianceInstance;
use arm_core::compliance_unit::ComplianceUnit;
use arm_core::delta_types::DeltaWitness;
use arm_core::logic_instance::{AppData, ExpirableBlob, LogicVerifierInputs};
use arm_core::transaction::{Delta, Transaction};
use arm_core::Digest;
use groth_16_verifier::Proof;
use verifier_router::Seal;
use verifier_router::Selector;

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

/// Arbitrary value; verifies the proof parser extracts the selector correctly.
pub const FAKE_SELECTOR: Selector = [0x31, 0x0f, 0xe5, 0x98];

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

/// One action, one CU, two LVIs (consumed + created).
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

/// Attaches payloads to the first LVI (index 0) of a minimal transaction.
/// Preserves CU-consistent LVI tags. For tests needing multiple LVIs or that
/// don't care about tag consistency, see `create_transaction_with_multi_lvi_payloads`.
pub fn create_transaction_with_external_payload(payloads: Vec<ExpirableBlob>) -> Transaction {
    let mut tx = create_minimal_transaction();
    tx.actions[0].logic_verifier_inputs[0]
        .app_data
        .external_payload = payloads;
    tx
}

/// One action, one CU, multiple LVIs with per-LVI external payloads.
///
/// The LVI list replaces the two LVIs from `create_minimal_transaction`, so LVI tags
/// do NOT correspond to the CU's nullifier/commitment. Only valid for external call
/// extraction tests — not for encoding/journal tests that require CU↔LVI consistency.
pub fn create_transaction_with_multi_lvi_payloads(
    payloads_per_lvi: Vec<Vec<ExpirableBlob>>,
) -> Transaction {
    let mut tx = create_minimal_transaction();
    tx.actions[0].logic_verifier_inputs = payloads_per_lvi
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
    tx
}

/// Variable-depth tree starting at depth 1 (capacity = 2 leaves).
pub fn create_test_pa_state() -> PAStateAccount {
    create_test_pa_state_with(Pubkey::default(), false)
}

pub fn create_test_pa_state_with(authority: Pubkey, stopped: bool) -> PAStateAccount {
    PAStateAccount {
        bump: 0,
        authority,
        pending_authority: None,
        verifier_router: Pubkey::default(),
        proof_selector: FAKE_SELECTOR,
        lifecycle: if stopped {
            PALifecycle::Stopped
        } else {
            PALifecycle::Running
        },
        root: EMPTY_TREE_ROOT_INITIAL.to_bytes(),
        next_index: 0,
        current_depth: INITIAL_TREE_DEPTH as u8,
        frontier: vec![ZEROS[0].to_bytes()],
        min_expiry_slots: MIN_EXPIRY_SLOTS,
        max_expiry_slots: MAX_EXPIRY_SLOTS,
    }
}

pub fn make_external_call(
    program_id: [u8; 32],
    instruction_data: Vec<u8>,
) -> crate::types::SolanaExternalCall {
    crate::types::SolanaExternalCall {
        program_id,
        instruction_data,
        expected_output: vec![],
        output_mode: crate::types::OutputMode::ReturnData,
    }
}
