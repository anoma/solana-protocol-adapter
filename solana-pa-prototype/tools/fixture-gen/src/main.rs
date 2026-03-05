use anchor_lang::prelude::AnchorDeserialize as BorshDeserialize;
use anyhow::{anyhow, Context, Result};
use arm::action::{Action, ActionExt};
use arm::action_tree::MerkleTree;
use arm::compliance::{
    initial_root, ComplianceInstanceJournalExt, ComplianceWitness,
};
use arm::compliance_unit::create_compliance_unit;
use arm::delta_proof::DeltaWitness;
use arm::logic_instance::ExpirableBlob;
use arm::logic_instance::{AppData, LogicInstance};
use arm::logic_proof::{LogicVerifier, LogicVerifierInputsExt};
use arm::merkle_path::MerklePath;
use arm::nullifier_key::{NullifierKey, NullifierKeyExt};
use arm::proving_system::{encode_seal, ProofType};
use arm::resource::Resource;
use arm::transaction::{Delta, Transaction, TransactionExt};
use arm::utils::{bytes_to_words, core_to_risc0_digest};
use arm::CoreDeltaWitness;
use arm::Digest;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use k256::Scalar;
use rayon::ThreadPoolBuilder;
use risc0_zkvm::sha::{Digestible as _, Sha256 as _};
use risc0_zkvm::{InnerReceipt, MaybePruned, Receipt, ReceiptClaim};
use serde::Serialize;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use verifier_router::Seal;

use passthrough_logic_methods::{PASSTHROUGH_LOGIC_GUEST_ELF, PASSTHROUGH_LOGIC_GUEST_ID};

fn hash_delta_msg(msg: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(msg).into()
}

use block_time_forwarder::{RESULT_GT, RESULT_LT};
use solana_pa::external_calls::encode_external_call;
use solana_pa::types::{OutputMode, SolanaExternalCall};
use test_forwarder::{MODE_FAIL, MODE_SILENT, MODE_WRITE_ACCOUNT};

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

struct CliArgs {
    threads: Option<usize>,
    debug_assumptions: bool,
    forwarder_mode: ForwarderMode,
    nonce_seed: Option<u8>,
    multi_external_call: bool,
    error_variants_dir: Option<PathBuf>,
    out_path: PathBuf,
}

enum ForwarderMode {
    BlockTimeForwarder { output_mismatch: bool },
    TestForwarderFail,
    TestForwarderSilent,
    TestForwarderOutputAccount,
}

/// Extract the Groth16 selector from a transaction's aggregation proof.
/// The selector is the first 4 bytes of the verifier_parameters digest,
/// which is the last 32 bytes of the serialized proof.
fn extract_selector(tx: &Transaction) -> Result<String> {
    let agg_proof = tx
        .aggregation_proof
        .as_ref()
        .ok_or_else(|| anyhow!("no aggregation_proof found for selector extraction"))?;

    let seal: Seal =
        Seal::try_from_slice(agg_proof).context("decode Seal from aggregation_proof bytes")?;
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

    let old_created_commitment = cu.instance.created_commitment;

    let mut new_bytes = [0u8; 32];
    new_bytes.copy_from_slice(old_created_commitment.as_bytes());
    new_bytes[0] ^= 1;
    let new_created_commitment = Digest::from_bytes(new_bytes);

    cu.instance.created_commitment = new_created_commitment;

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

fn decode_base58_32(s: &str) -> Result<[u8; 32]> {
    let bytes = bs58::decode(s)
        .into_vec()
        .with_context(|| format!("invalid base58: {s}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("expected 32 bytes, got {}", v.len()))
}

fn test_forwarder_program_id() -> Result<[u8; 32]> {
    decode_base58_32("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD")
}

fn block_time_forwarder_external_payload_blob(output_mismatch: bool) -> Result<ExpirableBlob> {
    // Must match `programs/block-time-forwarder/src/lib.rs::declare_id!`.
    let program_id = decode_base58_32("J1YYaBphwHzvGDq6EGY71DfPkuKWxMtHrtzGMUHp1LZ6")?;

    // Use -1 so expected_time < current_time for any reasonable cluster clock.
    // The forwarder will return RESULT_LT (0x00).
    let input = (-1_i64).to_le_bytes().to_vec();

    // If output_mismatch is true, set expected_output to RESULT_GT which is WRONG.
    // The forwarder will return RESULT_LT, but we expect RESULT_GT, causing ExternalCallOutputMismatch.
    let expected_output = if output_mismatch {
        vec![RESULT_GT]
    } else {
        vec![RESULT_LT]
    };

    Ok(encode_external_call(&SolanaExternalCall {
        program_id,
        instruction_data: input,
        expected_output,
        output_mode: OutputMode::ReturnData,
    }))
}

fn test_forwarder_fail_payload_blob() -> Result<ExpirableBlob> {
    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder_program_id()?,
        instruction_data: vec![MODE_FAIL],
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
    }))
}

fn test_forwarder_silent_payload_blob() -> Result<ExpirableBlob> {
    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder_program_id()?,
        instruction_data: vec![MODE_SILENT],
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
    }))
}

fn test_forwarder_output_account_payload_blob(
    expected_bytes: &[u8],
    account_index: u8,
) -> Result<ExpirableBlob> {
    // The forwarder strips input[0] (mode byte) and writes input[1..] to the account.
    let mut instruction_data = Vec::with_capacity(1 + expected_bytes.len());
    instruction_data.push(MODE_WRITE_ACCOUNT);
    instruction_data.extend_from_slice(expected_bytes);

    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder_program_id()?,
        instruction_data,
        expected_output: expected_bytes.to_vec(),
        output_mode: OutputMode::OutputAccount {
            index: account_index,
            offset: 0,
            len: expected_bytes.len() as u32,
        },
    }))
}

fn generate_test_transaction_with_external_payload(
    forwarder_mode: ForwarderMode,
    nonce_seed: Option<u8>,
    multi_external_call: bool,
) -> Result<Transaction> {
    // Inner proofs must be Succinct for aggregation.
    let base_proof_type = ProofType::Succinct;

    // Use the passthrough logic circuit for both consumed and created resources.
    // This allows us to bind arbitrary `app_data.external_payload` into real proofs.
    let passthrough_vk = Digest(PASSTHROUGH_LOGIC_GUEST_ID);

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
    let output_mismatch = matches!(
        &forwarder_mode,
        ForwarderMode::BlockTimeForwarder {
            output_mismatch: true
        }
    );
    let nonce_byte: u8 = nonce_seed.unwrap_or(if output_mismatch { 2 } else { 0 });
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
        nf_key,
        ephemeral_root: initial_root(),
    };
    let compliance_receipt =
        create_compliance_unit(&compliance_witness, base_proof_type).context("prove compliance")?;

    let tags = vec![consumed_nf, created_cm];
    let action_tree = MerkleTree::from(tags);
    let root = action_tree.root().context("compute action tree root")?;

    // Create app_data with a real external payload for the consumed logic instance only.
    let mut consumed_app_data = AppData::default();
    let external_blob = match &forwarder_mode {
        ForwarderMode::BlockTimeForwarder { output_mismatch } => {
            block_time_forwarder_external_payload_blob(*output_mismatch)?
        }
        ForwarderMode::TestForwarderFail => test_forwarder_fail_payload_blob()?,
        ForwarderMode::TestForwarderSilent => test_forwarder_silent_payload_blob()?,
        ForwarderMode::TestForwarderOutputAccount => {
            test_forwarder_output_account_payload_blob(b"\x01\x02\x03\x04", 2)?
        }
    };
    consumed_app_data.external_payload.push(external_blob);
    if multi_external_call {
        if let ForwarderMode::BlockTimeForwarder { .. } = &forwarder_mode {
            consumed_app_data
                .external_payload
                .push(block_time_forwarder_external_payload_blob(false)?);
        }
    }

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

    let tx = Transaction::create(
        vec![action],
        Delta::Witness(CoreDeltaWitness(delta_witness.to_bytes())),
    );
    let balanced_tx = tx
        .generate_delta_proof(hash_delta_msg)
        .context("generate delta proof")?;
    balanced_tx
        .clone()
        .verify(hash_delta_msg)
        .context("verify tx")?;

    Ok(balanced_tx)
}

fn generate_error_variant_fixtures(
    tx: &Transaction,
    selector: &str,
    nullifiers_b64: &[String],
    out_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create error variants dir {}", out_dir.display()))?;

    let write_variant = |file_name: &str, variant_tx: &Transaction| -> Result<()> {
        let tx_bytes = bincode::serialize(variant_tx)
            .with_context(|| format!("serialize variant tx for {file_name}"))?;
        let fixture = Fixture {
            format: "arm-risc0:Transaction(bincode)",
            aggregation_strategy: "batch",
            aggregation_proof_type: "groth16",
            selector: selector.to_owned(),
            tx_b64: BASE64.encode(tx_bytes),
            tx_tampered_b64: String::new(),
            consumed_nullifiers_b64: nullifiers_b64.to_vec(),
        };

        let out_path = out_dir.join(file_name);
        fs::write(&out_path, serde_json::to_vec_pretty(&fixture)?)
            .with_context(|| format!("write error variant fixture to {}", out_path.display()))?;
        Ok(())
    };

    {
        let mut wrong_root = tx.clone();
        let action = wrong_root
            .actions
            .get_mut(0)
            .ok_or_else(|| anyhow!("tx has no actions"))?;
        let cu = action
            .compliance_units
            .get_mut(0)
            .ok_or_else(|| anyhow!("tx has no compliance units"))?;
        cu.instance.consumed_commitment_tree_root = Digest::from_bytes([1u8; 32]);
        write_variant("wrong_root.json", &wrong_root)?;
    }

    {
        let mut agg_variant = tx.clone();
        agg_variant.aggregation_proof = None;
        write_variant("no_aggregation.json", &agg_variant)?;

        agg_variant.aggregation_proof = Some(vec![0xDE; 64]);
        write_variant("garbage_proof.json", &agg_variant)?;
    }

    Ok(())
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

fn timed_phase<F, T>(name: &str, f: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    eprintln!("phase: {name}");
    let start = Instant::now();
    let result = f()?;
    eprintln!("phase done: {name} ({})", fmt_duration(start.elapsed()));
    Ok(result)
}

fn print_usage() {
    eprintln!(
        "Usage:\n  fixture-gen [--threads N] [--debug-assumptions] [--output-mismatch] [--forwarder-fail] [--forwarder-silent] [--forwarder-output-account] [--nonce-seed N] [--multi-external-call] [--error-variants DIR] [OUT_PATH]\n\nExamples:\n  fixture-gen tests/fixtures/batch_groth16.json\n  fixture-gen --threads 4 tests/fixtures/batch_groth16.json\n  fixture-gen --debug-assumptions /tmp/batch_groth16.json\n  fixture-gen --output-mismatch tests/fixtures/batch_groth16_mismatch.json\n  fixture-gen --forwarder-fail tests/fixtures/batch_groth16_forwarder_fail.json\n  fixture-gen --forwarder-silent tests/fixtures/batch_groth16_forwarder_silent.json\n  fixture-gen --forwarder-output-account tests/fixtures/batch_groth16_forwarder_output_account.json\n  fixture-gen --nonce-seed 7 --multi-external-call /tmp/batch_groth16_multi.json\n  fixture-gen --error-variants tests/fixtures/error_variants tests/fixtures/batch_groth16.json\n\nNotes:\n  - `--threads` sets the global rayon thread pool size (must be set before proving starts).\n  - `RAYON_NUM_THREADS` can also be used; `--threads` wins.\n  - `--debug-assumptions` prints claim digests for composition debugging.\n  - `--output-mismatch` generates a block-time-forwarder fixture with intentionally wrong expected_output to test ExternalCallOutputMismatch.\n  - `--forwarder-fail` uses the test-forwarder with an instruction that fails before output checks.\n  - `--forwarder-silent` uses the test-forwarder with no return data so output comparison fails.\n  - `--forwarder-output-account` uses the test-forwarder with OutputAccount mode.\n  - At most one of `--output-mismatch`, `--forwarder-fail`, `--forwarder-silent`, `--forwarder-output-account` may be set.\n  - `--nonce-seed` overrides the deterministic nonce byte used to derive nullifiers.\n  - `--multi-external-call` appends a second block-time-forwarder external call blob when block-time-forwarder mode is selected.\n  - `--error-variants` writes wrong_root/no_aggregation/garbage_proof fixtures from the final aggregated tx.\n"
    );
}

fn parse_args() -> Result<CliArgs> {
    let mut args = env::args().skip(1);
    let mut threads: Option<usize> = None;
    let mut debug_assumptions = false;
    let mut forwarder_mode: Option<ForwarderMode> = None;
    let mut nonce_seed: Option<u8> = None;
    let mut multi_external_call = false;
    let mut error_variants_dir: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        // Handle positional arguments before splitting on '='.
        if !arg.starts_with('-') {
            if out_path.is_some() {
                return Err(anyhow!("unexpected extra argument: {arg}"));
            }
            out_path = Some(PathBuf::from(arg));
            continue;
        }

        // Support both "--flag value" and "--flag=value" uniformly.
        let (flag, eq_value) = match arg.find('=') {
            Some(pos) => (&arg[..pos], Some(&arg[pos + 1..])),
            None => (arg.as_str(), None),
        };

        match flag {
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            "--threads" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
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
            "--output-mismatch" | "--forwarder-fail" | "--forwarder-silent"
            | "--forwarder-output-account" => {
                if forwarder_mode.is_some() {
                    return Err(anyhow!(
                        "at most one of --output-mismatch, --forwarder-fail, --forwarder-silent, --forwarder-output-account may be set"
                    ));
                }
                forwarder_mode = Some(match flag {
                    "--output-mismatch" => ForwarderMode::BlockTimeForwarder {
                        output_mismatch: true,
                    },
                    "--forwarder-fail" => ForwarderMode::TestForwarderFail,
                    "--forwarder-silent" => ForwarderMode::TestForwarderSilent,
                    "--forwarder-output-account" => ForwarderMode::TestForwarderOutputAccount,
                    _ => unreachable!(),
                });
            }
            "--nonce-seed" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--nonce-seed requires a value"))?;
                let parsed = value
                    .parse::<u8>()
                    .with_context(|| format!("invalid --nonce-seed value: {value}"))?;
                nonce_seed = Some(parsed);
            }
            "--multi-external-call" => {
                multi_external_call = true;
            }
            "--error-variants" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--error-variants requires a value"))?;
                if value.is_empty() {
                    return Err(anyhow!(
                        "--error-variants requires a non-empty directory path"
                    ));
                }
                error_variants_dir = Some(PathBuf::from(value));
            }
            _ => {
                return Err(anyhow!("unknown flag: {arg}"));
            }
        }
    }

    let out_path = out_path
        .unwrap_or_else(|| PathBuf::from("solana-pa-prototype/tests/fixtures/batch_groth16.json"));

    let forwarder_mode = forwarder_mode.unwrap_or(ForwarderMode::BlockTimeForwarder {
        output_mismatch: false,
    });

    Ok(CliArgs {
        threads,
        debug_assumptions,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
        error_variants_dir,
        out_path,
    })
}

fn main() -> Result<()> {
    let total_start = Instant::now();
    let CliArgs {
        threads,
        debug_assumptions,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
        error_variants_dir,
        out_path,
    } = parse_args()?;

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
    if let ForwarderMode::BlockTimeForwarder {
        output_mismatch: true,
    } = &forwarder_mode
    {
        eprintln!(
            "mode: output-mismatch (intentionally wrong expected_output for ExternalCallOutputMismatch test)"
        );
    }
    if let Some(seed) = nonce_seed {
        eprintln!("mode: nonce-seed override ({seed})");
    }
    if multi_external_call {
        eprintln!("mode: multi-external-call (two external payload blobs)");
    }
    if let Some(dir) = &error_variants_dir {
        eprintln!("error variants output dir: {}", dir.display());
    }

    let mut tx = timed_phase("generate_test_transaction", || {
        generate_test_transaction_with_external_payload(forwarder_mode, nonce_seed, multi_external_call)
    })?;

    if debug_assumptions {
        timed_phase(
            "debug_assumptions (claim digests must match env::verify calls)",
            || debug_batch_assumptions(&tx),
        )?;
    }

    timed_phase(
        "aggregate_with_strategy(batch, groth16) (this is the expensive step)",
        || tx.aggregate(ProofType::Groth16).context("aggregate tx (batch, groth16)"),
    )?;

    timed_phase("verify_aggregation", || {
        tx.verify_aggregation().context("verify aggregated proof")
    })?;

    timed_phase("encode_seal", || {
        let agg_proof_bytes = tx.aggregation_proof.as_ref().unwrap();
        tx.aggregation_proof = Some(encode_seal(agg_proof_bytes).context("encode seal")?);
        Ok(())
    })?;

    let tx_bytes = timed_phase("serialize_tx", || {
        let bytes = bincode::serialize(&tx).context("serialize tx")?;
        eprintln!("  {} bytes", bytes.len());
        Ok(bytes)
    })?;

    let consumed_nullifiers_b64 = timed_phase("extract_nullifiers", || {
        let mut nuls = Vec::new();
        for action in &tx.actions {
            for cu in &action.compliance_units {
                nuls.push(BASE64.encode(cu.instance.consumed_nullifier.as_bytes()));
            }
        }
        Ok(nuls)
    })?;

    let (tx_tampered_bytes, selector) = timed_phase("tamper_and_extract_selector", || {
        let mut tx_tampered = tx.clone();
        mutate_created_commitment_keep_structure(&mut tx_tampered)?;
        let tampered_bytes =
            bincode::serialize(&tx_tampered).context("serialize tampered tx")?;
        eprintln!("  tampered: {} bytes", tampered_bytes.len());

        let sel = extract_selector(&tx).context("extract selector from proof")?;
        eprintln!("  selector: {sel}");
        Ok((tampered_bytes, sel))
    })?;

    let fixture = Fixture {
        format: "arm-risc0:Transaction(bincode)",
        aggregation_strategy: "batch",
        aggregation_proof_type: "groth16",
        selector,
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tx_tampered_bytes),
        consumed_nullifiers_b64,
    };

    timed_phase("write_fixture", || {
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
        }
        fs::write(&out_path, serde_json::to_vec_pretty(&fixture)?)
            .with_context(|| format!("write fixture to {}", out_path.display()))?;
        Ok(())
    })?;

    if let Some(dir) = error_variants_dir.as_deref() {
        timed_phase("write_error_variants", || {
            generate_error_variant_fixtures(
                &tx,
                &fixture.selector,
                &fixture.consumed_nullifiers_b64,
                dir,
            )
        })?;
    }

    eprintln!(
        "wrote fixture: {} (total {})",
        out_path.display(),
        fmt_duration(total_start.elapsed())
    );
    Ok(())
}

fn compute_expected_claim_digest(
    journal: &[u8],
    vk: &Digest,
) -> risc0_zkvm::sha::Digest {
    let words = arm::utils::bytes_to_words(journal);
    let padded_bytes = arm::utils::words_to_bytes(&words);
    let journal_digest = *risc0_zkvm::sha::Impl::hash_bytes(padded_bytes);
    let expected_claim = ReceiptClaim::ok(
        core_to_risc0_digest(vk),
        MaybePruned::Pruned(journal_digest),
    );
    expected_claim.digest()
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
            let journal = cu
                .instance
                .to_journal()
                .context("serialize compliance journal")?;
            let receipt = Receipt::new(inner, journal.clone());
            let receipt_claim_digest = receipt.claim().context("read compliance claim")?.digest();

            let expected_claim_digest =
                compute_expected_claim_digest(&journal, &arm::constants::COMPLIANCE_VK);

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
        let (tags, logics) = solana_pa::encoding::extract_tags_and_logic_refs(action);

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

            let expected_claim_digest =
                compute_expected_claim_digest(&verifier.instance, &verifier.verifying_key);

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_base58_32_system_program() {
        // Solana system program: "11111111111111111111111111111111" (32 '1's) = 32 zero bytes
        let result = decode_base58_32("11111111111111111111111111111111").unwrap();
        assert_eq!(result, [0u8; 32]);
    }

    #[test]
    fn decode_base58_32_wrong_length_returns_error() {
        assert!(decode_base58_32("1").is_err());
    }

    #[test]
    fn bytes_to_words_roundtrip() {
        let original = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let words = bytes_to_words(&original);
        let recovered = arm::utils::words_to_bytes(&words);
        assert_eq!(&recovered[..original.len()], &original[..]);
    }

    #[test]
    fn bytes_to_words_roundtrip_with_padding() {
        // 5 bytes: not a multiple of 4, so words_to_bytes will have 3 padding zeros
        let original = vec![0xAA, 0xBB, 0xCC, 0xDD, 0xEE];
        let words = bytes_to_words(&original);
        let recovered = arm::utils::words_to_bytes(&words);
        assert_eq!(&recovered[..original.len()], &original[..]);
        for &b in &recovered[original.len()..] {
            assert_eq!(b, 0, "padding bytes should be zero");
        }
    }
}
