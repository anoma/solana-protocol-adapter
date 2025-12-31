//! Proptest strategies for generating test inputs.
//!
//! These strategies generate random inputs for property-based testing,
//! mirroring the `bound()` approach used in the EVM Protocol Adapter's Foundry tests.

use proptest::prelude::*;
use crate::types::*;

/// Strategy for generating arbitrary 32-byte Digests.
pub fn arb_digest() -> impl Strategy<Value = Digest> {
    prop::array::uniform32(any::<u8>()).prop_map(Digest::from_bytes)
}

/// Strategy for non-zero Digests (excludes all-zeros).
pub fn arb_nonzero_digest() -> impl Strategy<Value = Digest> {
    prop::array::uniform32(1u8..=255u8).prop_map(Digest::from_bytes)
}

/// Strategy for byte vectors of bounded length.
pub fn arb_byte_vec(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max_len)
}

/// Strategy for word-aligned byte arrays (len % 4 == 0).
pub fn arb_aligned_bytes(max_words: usize) -> impl Strategy<Value = Vec<u8>> {
    (0..=max_words).prop_flat_map(|word_count| {
        prop::collection::vec(any::<u8>(), word_count * 4)
    })
}

/// Strategy for u32 word arrays.
pub fn arb_words(max_count: usize) -> impl Strategy<Value = Vec<u32>> {
    prop::collection::vec(any::<u32>(), 0..=max_count)
}

/// Strategy for ComplianceInstance with arbitrary deltas.
pub fn arb_compliance_instance() -> impl Strategy<Value = ComplianceInstance> {
    (
        arb_digest(),
        arb_digest(),
        arb_digest(),
        arb_digest(),
        arb_digest(),
        prop::array::uniform8(any::<u32>()),
        prop::array::uniform8(any::<u32>()),
    )
        .prop_map(|(nf, clr, ctr, cm, clr2, dx, dy)| ComplianceInstance {
            consumed_nullifier: nf,
            consumed_logic_ref: clr,
            consumed_commitment_tree_root: ctr,
            created_commitment: cm,
            created_logic_ref: clr2,
            delta_x: dx,
            delta_y: dy,
        })
}

/// Strategy for ComplianceInstance with zero deltas (identity point).
pub fn arb_compliance_instance_zero_delta() -> impl Strategy<Value = ComplianceInstance> {
    (
        arb_digest(),
        arb_digest(),
        arb_digest(),
        arb_digest(),
        arb_digest(),
    )
        .prop_map(|(nf, clr, ctr, cm, clr2)| ComplianceInstance {
            consumed_nullifier: nf,
            consumed_logic_ref: clr,
            consumed_commitment_tree_root: ctr,
            created_commitment: cm,
            created_logic_ref: clr2,
            delta_x: [0u32; 8],
            delta_y: [0u32; 8],
        })
}

/// Strategy for ExpirableBlob.
pub fn arb_expirable_blob(max_words: usize) -> impl Strategy<Value = ExpirableBlob> {
    (arb_words(max_words), any::<u32>()).prop_map(|(blob, dc)| ExpirableBlob {
        blob,
        deletion_criterion: dc,
    })
}

/// Strategy for AppData.
pub fn arb_app_data(max_blobs: usize, max_words: usize) -> impl Strategy<Value = AppData> {
    (
        prop::collection::vec(arb_expirable_blob(max_words), 0..=max_blobs),
        prop::collection::vec(arb_expirable_blob(max_words), 0..=max_blobs),
        prop::collection::vec(arb_expirable_blob(max_words), 0..=max_blobs),
        prop::collection::vec(arb_expirable_blob(max_words), 0..=max_blobs),
    )
        .prop_map(|(rp, dp, ep, ap)| AppData {
            resource_payload: rp,
            discovery_payload: dp,
            external_payload: ep,
            application_payload: ap,
        })
}

/// Strategy for OutputMode.
pub fn arb_output_mode() -> impl Strategy<Value = OutputMode> {
    prop_oneof![
        Just(OutputMode::ReturnData),
        (any::<u8>(), any::<u32>(), 1u32..=4096u32)
            .prop_map(|(idx, off, len)| OutputMode::OutputAccount {
                index: idx,
                offset: off,
                len
            })
    ]
}

/// Strategy for SolanaExternalCall.
pub fn arb_solana_external_call(max_data_len: usize) -> impl Strategy<Value = SolanaExternalCall> {
    (
        prop::array::uniform32(any::<u8>()),
        arb_byte_vec(max_data_len),
        arb_byte_vec(max_data_len),
        arb_output_mode(),
    )
        .prop_map(|(pid, input, output, mode)| SolanaExternalCall {
            program_id: pid,
            instruction_data: input,
            expected_output: output,
            output_mode: mode,
        })
}

/// Strategy for LogicVerifierInputs.
pub fn arb_logic_verifier_inputs() -> impl Strategy<Value = LogicVerifierInputs> {
    (arb_digest(), arb_digest(), arb_app_data(2, 8)).prop_map(|(tag, vk, app_data)| {
        LogicVerifierInputs {
            tag,
            verifying_key: vk,
            app_data,
            proof: None,
        }
    })
}

/// Strategy for minimal valid Transaction (1 action, 1 CU).
pub fn arb_minimal_transaction() -> impl Strategy<Value = Transaction> {
    arb_compliance_instance_zero_delta().prop_map(|instance| {
        let nf = instance.consumed_nullifier;
        let cm = instance.created_commitment;
        let clr = instance.consumed_logic_ref;
        let clr2 = instance.created_logic_ref;

        Transaction {
            actions: vec![Action {
                compliance_units: vec![ComplianceUnit {
                    instance: bincode::serialize(&instance).unwrap(),
                    proof: None,
                }],
                logic_verifier_inputs: vec![
                    LogicVerifierInputs {
                        tag: nf,
                        verifying_key: clr,
                        app_data: AppData::default(),
                        proof: None,
                    },
                    LogicVerifierInputs {
                        tag: cm,
                        verifying_key: clr2,
                        app_data: AppData::default(),
                        proof: None,
                    },
                ],
            }],
            delta_proof: Delta::Witness(vec![]),
            expected_balance: None,
            aggregation_proof: None,
        }
    })
}

/// Strategy for Transaction with N compliance units.
pub fn arb_transaction_with_n_cus(n: usize) -> impl Strategy<Value = Transaction> {
    prop::collection::vec(arb_compliance_instance_zero_delta(), n).prop_map(|instances| {
        let mut cus = Vec::with_capacity(instances.len());
        let mut lvis = Vec::with_capacity(instances.len() * 2);

        for inst in &instances {
            cus.push(ComplianceUnit {
                instance: bincode::serialize(inst).unwrap(),
                proof: None,
            });
            lvis.push(LogicVerifierInputs {
                tag: inst.consumed_nullifier,
                verifying_key: inst.consumed_logic_ref,
                app_data: AppData::default(),
                proof: None,
            });
            lvis.push(LogicVerifierInputs {
                tag: inst.created_commitment,
                verifying_key: inst.created_logic_ref,
                app_data: AppData::default(),
                proof: None,
            });
        }

        Transaction {
            actions: vec![Action {
                compliance_units: cus,
                logic_verifier_inputs: lvis,
            }],
            delta_proof: Delta::Witness(vec![]),
            expected_balance: None,
            aggregation_proof: None,
        }
    })
}

/// Helper to create a Transaction from a slice of ComplianceInstances.
pub fn build_tx_from_instances(instances: &[ComplianceInstance]) -> Transaction {
    let cus: Vec<ComplianceUnit> = instances
        .iter()
        .map(|inst| ComplianceUnit {
            instance: bincode::serialize(inst).unwrap(),
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
        });
        lvis.push(LogicVerifierInputs {
            tag: inst.created_commitment,
            verifying_key: inst.created_logic_ref,
            app_data: AppData::default(),
            proof: None,
        });
    }

    Transaction {
        actions: vec![Action {
            compliance_units: cus,
            logic_verifier_inputs: lvis,
        }],
        delta_proof: Delta::Witness(vec![]),
        expected_balance: None,
        aggregation_proof: None,
    }
}
