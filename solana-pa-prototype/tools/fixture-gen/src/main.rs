use anchor_lang::prelude::AnchorDeserialize;
use anyhow::{anyhow, Context, Result};
use arm::action::Action;
use arm::action_tree::MerkleTree;
use arm::aggregation::AggregationStrategy;
use arm::compliance::{ComplianceInstance, ComplianceWitness, INITIAL_ROOT};
use arm::compliance_unit::ComplianceUnit;
use arm::delta_proof::DeltaWitness;
use arm::logic_instance::ExpirableBlob;
use arm::logic_instance::{AppData, LogicInstance};
use arm::logic_proof::LogicVerifier;
use arm::merkle_path::MerklePath;
use arm::nullifier_key::NullifierKey;
use arm::proving_system::{encode_seal, ProofType};
use arm::resource::Resource;
use arm::transaction::{Delta, Transaction};
use arm::utils::bytes_to_words;
use arm::Digest;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use k256::Scalar;
use rayon::ThreadPoolBuilder;
use risc0_zkvm::sha::{Digestible as _, Sha256 as _};
use risc0_zkvm::{InnerReceipt, MaybePruned, Receipt, ReceiptClaim};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use verifier_router::Seal;

use passthrough_logic_methods::{PASSTHROUGH_LOGIC_GUEST_ELF, PASSTHROUGH_LOGIC_GUEST_ID};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct SolanaExternalCall {
    pub program_id: [u8; 32],
    pub instruction_data: Vec<u8>,
    pub expected_output: Vec<u8>,
    pub output_mode: OutputMode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
enum OutputMode {
    ReturnData,
    OutputAccount { index: u8, offset: u32, len: u32 },
}

#[derive(Serialize)]
struct Fixture {
    format: &'static str,
    aggregation_strategy: &'static str,
    aggregation_proof_type: &'static str,
    /// Groth16 verifier selector extracted from the proof's verifier_parameters.
    /// Format: "0x" + 4-byte hex (e.g., "0x73c457ba").
    selector: String,
    tx_b64: String,
    tx_tampered_b64: String,
    consumed_nullifiers_b64: Vec<String>,
}

/// Extract the Groth16 selector from a transaction's aggregation proof.
/// The selector is the first 4 bytes of the verifier_parameters digest,
/// which is the last 32 bytes of the serialized proof.
fn extract_selector(tx: &Transaction) -> Result<String> {
    let agg_proof = tx
        .aggregation_proof
        .as_ref()
        .ok_or_else(|| anyhow!("no aggregation_proof found for selector extraction"))?;

    let seal: Seal = Seal::try_from_slice(agg_proof).unwrap();
    Ok(format!("0x{}", hex::encode(seal.selector)))
}

fn mutate_created_commitment_keep_structure(tx: &mut Transaction) -> Result<()> {
    let action = tx
        .actions
        .get_mut(0)
        .ok_or_else(|| anyhow!("tx has no actions"))?;
    let cu = action
        .compliance_units
        .get_mut(0)
        .ok_or_else(|| anyhow!("tx has no compliance units"))?;

    let instance = cu.instance.clone();
    let old_created_commitment = instance.created_commitment;

    let mut new_bytes = [0u8; 32];
    new_bytes.copy_from_slice(old_created_commitment.as_bytes());
    new_bytes[0] ^= 1;
    let new_created_commitment = Digest::from_bytes(new_bytes);

    let mut new_instance = instance.clone();
    new_instance.created_commitment = new_created_commitment;
    cu.instance = new_instance;

    // Update the corresponding created logic verifier input tag so decoding and
    // digest computation still succeeds (proof must then fail).
    let mut updated = false;
    for lvi in action.logic_verifier_inputs.iter_mut() {
        if lvi.tag == old_created_commitment {
            lvi.tag = new_created_commitment;
            updated = true;
            break;
        }
    }
    if !updated {
        return Err(anyhow!(
            "could not find created logic verifier input for tamper"
        ));
    }

    Ok(())
}

fn b58_value(byte: u8) -> Option<u8> {
    match byte {
        b'1'..=b'9' => Some(byte - b'1'),
        b'A'..=b'H' => Some(byte - b'A' + 9),
        b'J'..=b'N' => Some(byte - b'J' + 17),
        b'P'..=b'Z' => Some(byte - b'P' + 22),
        b'a'..=b'k' => Some(byte - b'a' + 33),
        b'm'..=b'z' => Some(byte - b'm' + 44),
        _ => None,
    }
}

fn decode_base58(s: &str) -> Result<Vec<u8>> {
    // Minimal base58 decoder (Bitcoin alphabet) sufficient for Solana pubkeys.
    let mut bytes: Vec<u8> = Vec::new();
    for &ch in s.as_bytes() {
        let value = b58_value(ch).ok_or_else(|| anyhow!("invalid base58 char: {}", ch as char))?;

        let mut carry = value as u32;
        for b in bytes.iter_mut().rev() {
            let acc = (*b as u32) * 58 + carry;
            *b = (acc & 0xff) as u8;
            carry = acc >> 8;
        }
        while carry > 0 {
            bytes.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }

    let leading_zeros = s.as_bytes().iter().take_while(|&&c| c == b'1').count();
    if leading_zeros > 0 {
        let mut out = vec![0u8; leading_zeros];
        out.extend_from_slice(&bytes);
        bytes = out;
    }

    Ok(bytes)
}

fn decode_base58_32(s: &str) -> Result<[u8; 32]> {
    let bytes = decode_base58(s)?;
    if bytes.len() != 32 {
        return Err(anyhow!("expected 32 bytes, got {}", bytes.len()));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn block_time_forwarder_external_payload_blob(output_mismatch: bool) -> Result<ExpirableBlob> {
    // Must match `programs/block-time-forwarder/src/lib.rs::declare_id!`.
    let program_id = decode_base58_32("2H6dTRHG16qWYh2Dw7Znfhh7CviKkHzq8cWhKKQ7WtgN")?;

    // Use -1 so expected_time < current_time for any reasonable cluster clock.
    // The forwarder will return RESULT_LT (0x00).
    let input = (-1_i64).to_le_bytes().to_vec();

    // If output_mismatch is true, set expected_output to RESULT_GT (0x02) which is WRONG.
    // The forwarder will return 0x00 (LT), but we expect 0x02 (GT), causing ExternalCallOutputMismatch.
    let expected_output = if output_mismatch {
        vec![0x02] // RESULT_GT - intentionally wrong
    } else {
        vec![0x00] // RESULT_LT - correct
    };

    let call = SolanaExternalCall {
        program_id,
        instruction_data: input,
        expected_output,
        output_mode: OutputMode::ReturnData,
    };

    let call_bytes = bincode::serialize(&call).context("serialize SolanaExternalCall")?;
    Ok(ExpirableBlob {
        blob: bytes_to_words(&call_bytes),
        deletion_criterion: 0,
    })
}

fn generate_test_transaction_with_external_payload(output_mismatch: bool) -> Result<Transaction> {
    // Inner proofs must be Succinct for aggregation.
    let base_proof_type = ProofType::Succinct;

    // Use the passthrough logic circuit for both consumed and created resources.
    // This allows us to bind arbitrary `app_data.external_payload` into real proofs.
    let passthrough_vk: Digest = PASSTHROUGH_LOGIC_GUEST_ID.into();

    let nf_key = NullifierKey::default();
    let nf_key_cm = nf_key.commit();

    // Generate one consumed and one created resource.
    let mut consumed_resource = Resource {
        logic_ref: passthrough_vk,
        nk_commitment: nf_key_cm,
        quantity: 1,
        is_ephemeral: true,
        ..Default::default()
    };
    // Stable-ish nonce so the fixture is deterministic.
    // Use different nonce for each fixture variant so they have different nullifiers.
    // This prevents DuplicateNullifier errors when running multiple fixtures in a test suite.
    let nonce_byte: u8 = if output_mismatch { 2 } else { 0 };
    consumed_resource.nonce = [[nonce_byte; 16], [0u8; 16]].concat().try_into().unwrap();
    let consumed_nf = consumed_resource
        .nullifier(&nf_key)
        .context("compute consumed nullifier")?;

    let mut created_resource = consumed_resource;
    created_resource.set_nonce(consumed_nf);
    let created_cm = created_resource.commitment();

    // Create ComplianceWitness with fixed rcv for deterministic fixtures.
    // The consumed resource is ephemeral, so use empty merkle_path and INITIAL_ROOT.
    let compliance_witness = ComplianceWitness {
        consumed_resource,
        created_resource,
        merkle_path: MerklePath::empty(),
        rcv: Scalar::ONE.to_bytes().to_vec(),
        nf_key: nf_key.clone(),
        ephemeral_root: *INITIAL_ROOT,
    };
    let compliance_receipt =
        ComplianceUnit::create(&compliance_witness, base_proof_type).context("prove compliance")?;

    let tags = vec![consumed_nf, created_cm];
    let action_tree = MerkleTree::from(tags.clone());
    let root = action_tree.root().context("compute action tree root")?;

    // Create app_data with a real external payload for the consumed logic instance only.
    let mut consumed_app_data = AppData::default();
    consumed_app_data
        .external_payload
        .push(block_time_forwarder_external_payload_blob(output_mismatch)?);

    let consumed_instance = LogicInstance {
        tag: consumed_nf,
        is_consumed: true,
        root,
        app_data: consumed_app_data,
    };
    let created_instance = LogicInstance {
        tag: created_cm,
        is_consumed: false,
        root,
        app_data: AppData::default(),
    };

    let (consumed_proof, consumed_journal) = arm::proving_system::prove(
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &consumed_instance,
        base_proof_type,
    )
    .context("prove consumed passthrough logic")?;
    let (created_proof, created_journal) = arm::proving_system::prove(
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &created_instance,
        base_proof_type,
    )
    .context("prove created passthrough logic")?;

    let consumed_logic = LogicVerifier {
        proof: Some(consumed_proof),
        instance: consumed_journal,
        verifying_key: passthrough_vk,
    };
    let created_logic = LogicVerifier {
        proof: Some(created_proof),
        instance: created_journal,
        verifying_key: passthrough_vk,
    };

    let action = Action::new(
        vec![compliance_receipt],
        vec![consumed_logic, created_logic],
    )
    .context("build action")?;

    // Delta witness is derived from compliance witness RCVs.
    let delta_witness =
        DeltaWitness::from_bytes_vec(&[compliance_witness.rcv]).context("build delta witness")?;

    let tx = Transaction::create(vec![action], Delta::Witness(delta_witness));
    let balanced_tx = tx.generate_delta_proof().context("generate delta proof")?;
    balanced_tx.clone().verify().context("verify tx")?;

    Ok(balanced_tx)
}

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    if secs < 60 {
        return format!("{secs}.{millis:03}s");
    }
    let mins = secs / 60;
    let rem = secs % 60;
    if mins < 60 {
        return format!("{mins}m{rem:02}.{millis:03}s");
    }
    let hours = mins / 60;
    let rem_mins = mins % 60;
    format!("{hours}h{rem_mins:02}m{rem:02}.{millis:03}s")
}

fn print_usage_and_exit() -> Result<()> {
    eprintln!(
        "Usage:\n  fixture-gen [--threads N] [--debug-assumptions] [--output-mismatch] [OUT_PATH]\n\nExamples:\n  fixture-gen tests/fixtures/batch_groth16.json\n  fixture-gen --threads 4 tests/fixtures/batch_groth16.json\n  fixture-gen --debug-assumptions /tmp/batch_groth16.json\n  fixture-gen --output-mismatch tests/fixtures/batch_groth16_mismatch.json\n\nNotes:\n  - `--threads` sets the global rayon thread pool size (must be set before proving starts).\n  - `RAYON_NUM_THREADS` can also be used; `--threads` wins.\n  - `--debug-assumptions` prints claim digests for composition debugging.\n  - `--output-mismatch` generates a fixture with intentionally wrong expected_output to test ExternalCallOutputMismatch.\n"
    );
    Ok(())
}

fn parse_args() -> Result<(Option<usize>, bool, bool, PathBuf)> {
    let mut args = env::args().skip(1);
    let mut threads: Option<usize> = None;
    let mut debug_assumptions = false;
    let mut output_mismatch = false;
    let mut out_path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print_usage_and_exit()?;
                std::process::exit(0);
            }
            "--threads" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--threads requires a value"))?;
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --threads value: {value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("--threads must be >= 1"));
                }
                threads = Some(parsed);
            }
            "--debug-assumptions" => {
                debug_assumptions = true;
            }
            "--output-mismatch" => {
                output_mismatch = true;
            }
            _ if arg.starts_with("--threads=") => {
                let value = arg.split_once('=').map(|(_, v)| v).unwrap_or_default();
                let parsed = value
                    .parse::<usize>()
                    .with_context(|| format!("invalid --threads value: {value}"))?;
                if parsed == 0 {
                    return Err(anyhow!("--threads must be >= 1"));
                }
                threads = Some(parsed);
            }
            _ if arg.starts_with('-') => {
                return Err(anyhow!("unknown flag: {arg}"));
            }
            _ => {
                if out_path.is_some() {
                    return Err(anyhow!("unexpected extra argument: {arg}"));
                }
                out_path = Some(PathBuf::from(arg));
            }
        }
    }

    let out_path = out_path
        .unwrap_or_else(|| PathBuf::from("solana-pa-prototype/tests/fixtures/batch_groth16.json"));

    Ok((threads, debug_assumptions, output_mismatch, out_path))
}

fn main() -> Result<()> {
    let total_start = Instant::now();
    let (threads, debug_assumptions, output_mismatch, out_path) = parse_args()?;

    // Configure rayon parallelism deterministically (helps avoid pegging/overheating/OOM).
    // Must happen before any proving work starts.
    if let Some(n) = threads {
        ThreadPoolBuilder::new()
            .num_threads(n)
            .build_global()
            .with_context(|| "failed to initialize global rayon thread pool")?;
        eprintln!("using rayon threads: {n}");
    } else if let Ok(n) = env::var("RAYON_NUM_THREADS") {
        eprintln!("RAYON_NUM_THREADS={n}");
    }

    eprintln!("fixture output: {}", out_path.display());
    eprintln!("mode: aggregated (batch Groth16)");
    if output_mismatch {
        eprintln!(
            "mode: output-mismatch (intentionally wrong expected_output for ExternalCallOutputMismatch test)"
        );
    }

    eprintln!("phase: generate_test_transaction");
    let start = Instant::now();
    let mut tx = generate_test_transaction_with_external_payload(output_mismatch)?;
    eprintln!(
        "phase done: generate_test_transaction ({})",
        fmt_duration(start.elapsed())
    );

    if debug_assumptions {
        eprintln!("phase: debug_assumptions (claim digests must match env::verify calls)");
        let start = Instant::now();
        debug_batch_assumptions(&tx)?;
        eprintln!(
            "phase done: debug_assumptions ({})",
            fmt_duration(start.elapsed())
        );
    }

    eprintln!("phase: aggregate_with_strategy(batch, groth16) (this is the expensive step)");
    let start = Instant::now();
    tx.aggregate_with_strategy(AggregationStrategy::Batch, ProofType::Groth16)
        .context("aggregate tx (batch, groth16)")?;
    eprintln!(
        "phase done: aggregate_with_strategy(batch, groth16) ({})",
        fmt_duration(start.elapsed())
    );

    eprintln!("phase: verify_aggregation");
    let start = Instant::now();
    tx.verify_aggregation().context("verify aggregated proof")?;
    eprintln!(
        "phase done: verify_aggregation ({})",
        fmt_duration(start.elapsed())
    );

    eprintln!("phase: encode seal");
    let proof = tx.aggregation_proof.as_ref().unwrap();
    tx.aggregation_proof = Some(encode_seal(proof).unwrap());
    eprintln!("phase done: encode seal");

    eprintln!("phase: serialize_tx");
    let start = Instant::now();
    let tx_bytes = bincode::serialize(&tx).context("serialize tx")?;
    eprintln!(
        "phase done: serialize_tx ({}, {} bytes)",
        fmt_duration(start.elapsed()),
        tx_bytes.len()
    );

    eprintln!("phase: extract_nullifiers");
    let start = Instant::now();
    let mut consumed_nullifiers_b64 = Vec::new();
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let instance = cu.instance.clone();
            consumed_nullifiers_b64.push(BASE64.encode(instance.consumed_nullifier.as_bytes()));
        }
    }
    eprintln!(
        "phase done: extract_nullifiers ({})",
        fmt_duration(start.elapsed())
    );

    eprintln!("phase: tamper_tx_and_serialize");
    let start = Instant::now();
    let mut tx_tampered = tx.clone();
    mutate_created_commitment_keep_structure(&mut tx_tampered)?;
    let tx_tampered_bytes = bincode::serialize(&tx_tampered).context("serialize tampered tx")?;
    eprintln!(
        "phase done: tamper_tx_and_serialize ({}, {} bytes)",
        fmt_duration(start.elapsed()),
        tx_tampered_bytes.len()
    );

    eprintln!("phase: extract_selector");
    let start = Instant::now();
    let selector = extract_selector(&tx).context("extract selector from proof")?;
    eprintln!(
        "phase done: extract_selector ({}, selector={})",
        fmt_duration(start.elapsed()),
        selector
    );

    let fixture = Fixture {
        format: "arm-risc0:Transaction(bincode)",
        aggregation_strategy: "batch",
        aggregation_proof_type: "groth16",
        selector,
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tx_tampered_bytes),
        consumed_nullifiers_b64,
    };

    eprintln!("phase: write_fixture");
    let start = Instant::now();
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
    }
    fs::write(&out_path, serde_json::to_vec_pretty(&fixture)?)
        .with_context(|| format!("write fixture to {}", out_path.display()))?;
    eprintln!(
        "phase done: write_fixture ({})",
        fmt_duration(start.elapsed())
    );

    eprintln!(
        "wrote fixture: {} (total {})",
        out_path.display(),
        fmt_duration(total_start.elapsed())
    );
    Ok(())
}

fn debug_batch_assumptions(tx: &Transaction) -> Result<()> {
    // The batch aggregation guest calls:
    // - env::verify(COMPLIANCE_VK, &ci_words)
    // - env::verify(logic_vk_i, &logic_instance_words_i)
    //
    // If any of these claim digests don't match the receipts provided via ExecutorEnv::add_assumption,
    // proving will fail with "no receipt found to resolve assumption".

    // Print a small header so the important line can be grepped.
    eprintln!("debug_assumptions: start");

    let mut compliance_idx = 0usize;
    for action in &tx.actions {
        for cu in &action.compliance_units {
            let Some(proof_bytes) = &cu.proof else {
                eprintln!("debug_assumptions: compliance[{compliance_idx}] proof is None");
                compliance_idx += 1;
                continue;
            };

            let inner: InnerReceipt =
                bincode::deserialize(proof_bytes).context("decode compliance InnerReceipt")?;
            let receipt = Receipt::new(inner, cu.instance.to_journal().unwrap());
            let receipt_claim_digest = receipt.claim().context("read compliance claim")?.digest();

            let words = arm::utils::bytes_to_words(&cu.instance.to_journal().unwrap());
            let padded_bytes = arm::utils::words_to_bytes(&words);
            let journal_digest = *risc0_zkvm::sha::Impl::hash_bytes(padded_bytes);
            let expected_claim = ReceiptClaim::ok(
                *arm::constants::COMPLIANCE_VK,
                MaybePruned::Pruned(journal_digest),
            );
            let expected_claim_digest = expected_claim.digest();

            eprintln!(
                "debug_assumptions: receipt_claim_digest={} expected_claim_digest={}",
                receipt_claim_digest, expected_claim_digest
            );

            compliance_idx += 1;
        }
    }

    let mut logic_idx = 0usize;
    for (action_idx, action) in tx.actions.iter().enumerate() {
        // Mirror `arm::action::Action::get_logic_verifiers` to derive the logic instances that the
        // batch aggregation circuit verifies.
        let compliance_instances: Vec<ComplianceInstance> = action
            .compliance_units
            .iter()
            .map(|cu| cu.instance.clone())
            .collect();

        let tags: Vec<risc0_zkvm::Digest> = compliance_instances
            .iter()
            .flat_map(|instance| vec![instance.consumed_nullifier, instance.created_commitment])
            .collect();
        let logics: Vec<risc0_zkvm::Digest> = compliance_instances
            .iter()
            .flat_map(|instance| vec![instance.consumed_logic_ref, instance.created_logic_ref])
            .collect();

        let action_tree = arm::action_tree::MerkleTree::from(tags.clone());
        let root = action_tree.root().context("compute action tree root")?;

        if tags.len() != action.logic_verifier_inputs.len() {
            return Err(anyhow!(
                "action[{action_idx}] tag count {} != logic_verifier_inputs {}",
                tags.len(),
                action.logic_verifier_inputs.len()
            ));
        }

        for (index, (tag, expected_vk)) in tags.iter().zip(logics.iter()).enumerate() {
            let input = action
                .logic_verifier_inputs
                .iter()
                .find(|input| &input.tag == tag)
                .ok_or_else(|| anyhow!("action[{action_idx}] missing logic input for tag"))?;
            if input.verifying_key != *expected_vk {
                return Err(anyhow!(
                    "action[{action_idx}] verifying key mismatch for tag"
                ));
            }

            let is_consumed = index % 2 == 0;
            let verifier = input
                .clone()
                .to_logic_verifier(is_consumed, root)
                .context("build logic verifier (instance bytes)")?;

            let Some(proof_bytes) = &verifier.proof else {
                eprintln!("debug_assumptions: logic[{logic_idx}] proof is None");
                logic_idx += 1;
                continue;
            };

            let inner: InnerReceipt =
                bincode::deserialize(proof_bytes).context("decode logic InnerReceipt")?;
            let receipt = Receipt::new(inner, verifier.instance.clone());
            let receipt_claim_digest = receipt.claim().context("read logic claim")?.digest();

            let words = arm::utils::bytes_to_words(&verifier.instance);
            let padded_bytes = arm::utils::words_to_bytes(&words);
            let journal_digest = *risc0_zkvm::sha::Impl::hash_bytes(padded_bytes);
            let expected_claim =
                ReceiptClaim::ok(verifier.verifying_key, MaybePruned::Pruned(journal_digest));
            let expected_claim_digest = expected_claim.digest();

            eprintln!(
                "debug_assumptions: logic[{logic_idx}] instance_len={} (mod4={}) receipt_claim_digest={} expected_claim_digest={}",
                verifier.instance.len(),
                verifier.instance.len() % 4,
                receipt_claim_digest,
                expected_claim_digest
            );

            logic_idx += 1;
        }
    }

    eprintln!("debug_assumptions: end");
    Ok(())
}
