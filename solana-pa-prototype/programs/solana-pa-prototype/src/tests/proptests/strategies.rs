use crate::types::{OutputMode, SolanaExternalCall};
use arm_core::compliance::ComplianceInstance;
use arm_core::Digest;
use proptest::prelude::*;

pub fn arb_digest() -> impl Strategy<Value = Digest> {
    prop::array::uniform32(any::<u8>()).prop_map(Digest::from_bytes)
}

pub fn arb_byte_vec(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max_len)
}

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
        .prop_map(
            |(nf, clr, ctr, cm, created_lr, dx, dy)| ComplianceInstance {
                consumed_nullifier: nf,
                consumed_logic_ref: clr,
                consumed_commitment_tree_root: ctr,
                created_commitment: cm,
                created_logic_ref: created_lr,
                delta_x: dx,
                delta_y: dy,
            },
        )
}

pub fn arb_solana_external_call(max_data_len: usize) -> impl Strategy<Value = SolanaExternalCall> {
    (
        prop::array::uniform32(any::<u8>()),
        arb_byte_vec(max_data_len),
        prop::collection::vec(any::<u8>(), 1..=max_data_len),
        1..=10u8,
    )
        .prop_map(|(pid, input, output, num_accounts)| SolanaExternalCall {
            program_id: pid,
            instruction_data: input,
            expected_output: output,
            output_mode: OutputMode::ReturnData,
            num_accounts,
        })
}
