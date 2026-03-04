use crate::types::*;
use proptest::prelude::*;

pub fn arb_digest() -> impl Strategy<Value = Digest> {
    prop::array::uniform32(any::<u8>()).prop_map(Digest::from_bytes)
}

pub fn arb_byte_vec(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..=max_len)
}

/// Word-aligned byte arrays (len % 4 == 0).
pub fn arb_aligned_bytes(max_words: usize) -> impl Strategy<Value = Vec<u8>> {
    (0..=max_words).prop_flat_map(|word_count| prop::collection::vec(any::<u8>(), word_count * 4))
}

pub fn arb_words(max_count: usize) -> impl Strategy<Value = Vec<u32>> {
    prop::collection::vec(any::<u32>(), 0..=max_count)
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

pub fn arb_expirable_blob(max_words: usize) -> impl Strategy<Value = ExpirableBlob> {
    (arb_words(max_words), any::<u32>()).prop_map(|(blob, dc)| ExpirableBlob {
        blob,
        deletion_criterion: dc,
    })
}

fn arb_output_mode() -> impl Strategy<Value = OutputMode> {
    prop_oneof![
        Just(OutputMode::ReturnData),
        (any::<u8>(), any::<u32>(), 1u32..=4096u32).prop_map(|(idx, off, len)| {
            OutputMode::OutputAccount {
                index: idx,
                offset: off,
                len,
            }
        })
    ]
}

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
