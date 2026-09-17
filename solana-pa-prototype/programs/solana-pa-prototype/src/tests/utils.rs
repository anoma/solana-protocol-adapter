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

/// Like `make_account_info!` but with caller-supplied account data, for testing
/// the reject-on-data branch of marker creation.
macro_rules! make_account_info_with_data {
    ($name:ident, $key:expr, owner: $owner:expr, lamports: $lamports:expr,
     data: $data:expr, signer: $signer:expr, writable: $writable:expr,
     executable: $executable:expr) => {
        #[allow(unused_mut)]
        let mut $name = ($lamports as u64, $data);
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
pub(crate) use make_account_info_with_data;

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

use crate::merkle::EMPTY_TREE_ROOT_INITIAL;
use crate::state::{PALifecycle, PAStateAccount};
use arm_core::aggregation_instance::{
    ActionAggregated, AggregationInstance, ConsumedResourceAggregated, CreatedResourceAggregated,
};
use arm_core::compliance::hash_kind_table_entries;
use arm_core::delta_proof::DeltaProof;
use arm_core::logic_instance::{AppData, ExpirableBlob};
use arm_core::transaction::{Aggregation, Delta, Transaction};
use arm_core::Digest;
use groth_16_verifier::Proof;
use verifier_router::Seal;
use verifier_router::Selector;

/// Commitment of the empty kind table — what a deployment with no
/// pre-registered kinds pins at initialization.
pub fn empty_kind_table_commitment() -> Digest {
    hash_kind_table_entries(&[])
}

/// One action with one consumed and one created resource, anchored to the
/// initial tree root and pinned to the compliance VK and empty kind table.
pub fn minimal_instance() -> AggregationInstance {
    AggregationInstance {
        compliance_key: arm_core::constants::COMPLIANCE_VK,
        kind_table_commitment: empty_kind_table_commitment(),
        actions: vec![ActionAggregated {
            consumed_publics: vec![ConsumedResourceAggregated {
                resource_nullifier: Digest::from_bytes([1u8; 32]),
                resource_logic_ref: Digest::from_bytes([3u8; 32]),
                commitment_tree_root: EMPTY_TREE_ROOT_INITIAL,
                app_data: AppData::default(),
            }],
            created_publics: vec![CreatedResourceAggregated {
                resource_commitment: Digest::from_bytes([2u8; 32]),
                resource_logic_ref: Digest::from_bytes([4u8; 32]),
                app_data: AppData::default(),
            }],
            delta_x: [0u32; 8],
            delta_y: [0u32; 8],
            action_tree_root: Digest::from_bytes([5u8; 32]),
        }],
    }
}

/// A wire-valid but cryptographically meaningless delta proof: r = s = 1
/// (in range and low-s), recovery id 0. All-zero components would be
/// rejected by `DeltaProof`'s deserializer, so fixtures cannot use them.
pub fn dummy_delta_proof() -> DeltaProof {
    let mut signature = [0u8; 64];
    signature[31] = 1; // r = 1
    signature[63] = 1; // s = 1
    DeltaProof {
        signature,
        recovery_id: 0,
    }
}

/// A minimal instance as an aggregated wire transaction: no base actions, a
/// structurally valid (but cryptographically meaningless) delta proof, and a
/// fake seal as the aggregation proof bytes.
pub fn create_minimal_transaction() -> Transaction {
    Transaction {
        actions: None,
        delta_proof: Delta::Proof(dummy_delta_proof()),
        expected_balance: None,
        aggregation: Some(Aggregation {
            proof: fake_aggregation_proof_bytes(),
            instance: minimal_instance(),
        }),
    }
}

/// Attaches external payloads to the consumed resource of a minimal instance.
pub fn instance_with_external_payload(payloads: Vec<ExpirableBlob>) -> AggregationInstance {
    instance_with_consumed_and_created_payloads(payloads, vec![])
}

/// One action whose consumed and created resources each carry their own
/// external payloads, for order-sensitive extraction tests.
pub fn instance_with_consumed_and_created_payloads(
    consumed_payloads: Vec<ExpirableBlob>,
    created_payloads: Vec<ExpirableBlob>,
) -> AggregationInstance {
    let mut instance = minimal_instance();
    instance.actions[0].consumed_publics[0]
        .app_data
        .external_payload = consumed_payloads;
    instance.actions[0].created_publics[0]
        .app_data
        .external_payload = created_payloads;
    instance
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

/// Variable-depth tree starting at depth 1 (capacity = 2 leaves).
pub fn create_test_pa_state() -> PAStateAccount {
    create_test_pa_state_with(Pubkey::default(), false)
}

pub fn create_test_pa_state_with(authority: Pubkey, stopped: bool) -> PAStateAccount {
    let mut state = PAStateAccount::running(
        0,
        authority,
        Pubkey::default(),
        FAKE_SELECTOR,
        empty_kind_table_commitment().into(),
    );
    if stopped {
        state.lifecycle = PALifecycle::Stopped;
    }
    state
}
