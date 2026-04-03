//! Benchmark: cost of borsh-serializing AppData + SHA256 hashing on-chain.
//!
//! Measures serialization output sizes for realistic AppData payloads
//! and estimates Solana compute unit costs.
//!
//! Solana SHA256 syscall cost: 100 CU base + ~2 CU per byte
//! Borsh serialization: pure BPF computation, ~1-3 CU per byte output
//! (heap allocation cost dominates for larger payloads)

use arm_core::logic_instance::{AppData, ExpirableBlob};

use crate::external_calls::encode_external_call;
use crate::types::{OutputMode, SolanaExternalCall};

/// Borsh-serialize AppData and return (serialized_bytes, byte_count).
fn borsh_serialize_app_data(app_data: &AppData) -> (Vec<u8>, usize) {
    let bytes = borsh::to_vec(app_data).expect("borsh serialization should succeed");
    let len = bytes.len();
    (bytes, len)
}

/// Estimate CU cost for borsh serialization + SHA256.
/// SHA256 syscall: 100 base + ~2 per byte (from Solana runtime)
/// Borsh serialization: estimated ~3 CU per byte (allocation + memcpy on BPF)
fn estimate_cu(serialized_bytes: usize) -> usize {
    let sha256_cu = 100 + (serialized_bytes * 2);
    // Borsh serialization on BPF: Vec allocation + field writes.
    // Conservative estimate: 5 CU per byte for the serialization step.
    let borsh_cu = serialized_bytes * 5;
    sha256_cu + borsh_cu
}

// -- Helpers to build realistic AppData --

fn make_block_time_forwarder_call() -> SolanaExternalCall {
    SolanaExternalCall {
        program_id: [0x11; 32],
        instruction_data: (-1_i64).to_le_bytes().to_vec(), // 8 bytes
        expected_output: vec![0x01],                       // 1 byte
        output_mode: OutputMode::ReturnData,
    }
}

fn make_spl_wrap_call() -> SolanaExternalCall {
    // SPL token forwarder wrap: opcode(1) + WrapInput(185 bytes)
    let mut instruction_data = vec![0x01]; // OP_WRAP
    instruction_data.extend_from_slice(&[0u8; 185]); // WrapInput placeholder
    SolanaExternalCall {
        program_id: [0x22; 32],
        instruction_data,
        expected_output: vec![0x01],
        output_mode: OutputMode::ReturnData,
    }
}

fn make_spl_unwrap_call() -> SolanaExternalCall {
    // SPL token forwarder unwrap: opcode(1) + token_mint(32) + amount(8) + recipient(32)
    let mut instruction_data = vec![0x02]; // OP_UNWRAP
    instruction_data.extend_from_slice(&[0u8; 72]); // UnwrapInput placeholder
    SolanaExternalCall {
        program_id: [0x22; 32],
        instruction_data,
        expected_output: vec![0x01],
        output_mode: OutputMode::ReturnData,
    }
}

fn app_data_with_calls(calls: Vec<SolanaExternalCall>) -> AppData {
    let mut app_data = AppData::new();
    for call in calls {
        app_data.add_external_payload(encode_external_call(&call));
    }
    app_data
}

// -- Benchmark tests --

#[test]
fn benchmark_empty_app_data() {
    let app_data = AppData::new();
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!("Empty AppData: {} bytes borsh, ~{} CU estimated", size, cu);

    // Empty AppData = 4 Vecs each with u32 length prefix (0) = 16 bytes
    assert_eq!(size, 16, "4 empty Vecs = 4 * 4-byte length prefix");
}

#[test]
fn benchmark_block_time_forwarder_app_data() {
    // Consumed LVI: one external call (block-time forwarder)
    let app_data = app_data_with_calls(vec![make_block_time_forwarder_call()]);
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!(
        "Block-time forwarder AppData (1 call): {} bytes borsh, ~{} CU estimated",
        size, cu
    );
}

#[test]
fn benchmark_spl_wrap_app_data() {
    let app_data = app_data_with_calls(vec![make_spl_wrap_call()]);
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!(
        "SPL wrap AppData (1 call): {} bytes borsh, ~{} CU estimated",
        size, cu
    );
}

#[test]
fn benchmark_spl_unwrap_app_data() {
    let app_data = app_data_with_calls(vec![make_spl_unwrap_call()]);
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!(
        "SPL unwrap AppData (1 call): {} bytes borsh, ~{} CU estimated",
        size, cu
    );
}

#[test]
fn benchmark_multi_call_app_data() {
    // Two external calls in one LVI
    let app_data = app_data_with_calls(vec![
        make_block_time_forwarder_call(),
        make_block_time_forwarder_call(),
    ]);
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!(
        "Multi-call AppData (2 calls): {} bytes borsh, ~{} CU estimated",
        size, cu
    );
}

#[test]
fn benchmark_worst_case_app_data() {
    // Worst realistic case: SPL wrap + unwrap in one LVI + resource/discovery payloads
    let mut app_data = app_data_with_calls(vec![make_spl_wrap_call(), make_spl_unwrap_call()]);
    // Add some resource and discovery payloads
    app_data.add_resource_payload(ExpirableBlob {
        blob: vec![0u32; 64], // 256 bytes of resource data
        deletion_criterion: 100,
    });
    app_data.add_discovery_payload(ExpirableBlob {
        blob: vec![0u32; 32], // 128 bytes of discovery data
        deletion_criterion: 50,
    });
    let (_, size) = borsh_serialize_app_data(&app_data);
    let cu = estimate_cu(size);
    println!(
        "Worst-case AppData (2 calls + resource + discovery): {} bytes borsh, ~{} CU estimated",
        size, cu
    );
}

/// Summary: total estimated CU for a typical 1-CU transaction (2 LVIs).
#[test]
fn benchmark_typical_transaction_total() {
    // Typical: 1 compliance unit = 2 LVIs (consumed + created).
    // Consumed LVI has external call, created LVI is empty.
    let consumed_app_data = app_data_with_calls(vec![make_block_time_forwarder_call()]);
    let created_app_data = AppData::new();

    let (_, consumed_size) = borsh_serialize_app_data(&consumed_app_data);
    let (_, created_size) = borsh_serialize_app_data(&created_app_data);

    let consumed_cu = estimate_cu(consumed_size);
    let created_cu = estimate_cu(created_size);
    let total_cu = consumed_cu + created_cu;

    println!("--- Typical 1-CU transaction (2 LVIs) ---");
    println!(
        "  Consumed LVI: {} bytes, ~{} CU",
        consumed_size, consumed_cu
    );
    println!("  Created LVI:  {} bytes, ~{} CU", created_size, created_cu);
    println!("  Total app_data hash cost: ~{} CU", total_cu);
    println!(
        "  As % of 1.4M CU budget: {:.2}%",
        (total_cu as f64 / 1_400_000.0) * 100.0
    );

    // Also measure with SPL forwarder calls
    let spl_consumed = app_data_with_calls(vec![make_spl_unwrap_call()]);
    let (_, spl_size) = borsh_serialize_app_data(&spl_consumed);
    let spl_cu = estimate_cu(spl_size) + created_cu;
    println!("\n--- SPL unwrap transaction (2 LVIs) ---");
    println!("  Total app_data hash cost: ~{} CU", spl_cu);
    println!(
        "  As % of 1.4M CU budget: {:.2}%",
        (spl_cu as f64 / 1_400_000.0) * 100.0
    );
}
