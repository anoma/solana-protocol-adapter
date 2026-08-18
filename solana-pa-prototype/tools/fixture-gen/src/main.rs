use anchor_lang::prelude::{AnchorDeserialize as BorshDeserialize, AnchorSerialize, Pubkey};
use anyhow::{anyhow, bail, Context, Result};
use arm::action::{Action, ActionExt};
use arm::action_tree::MerkleTree;
use arm::compliance::{initial_root, ComplianceInstance, ComplianceWitness};
use arm::compliance_unit::{create_compliance_unit, ComplianceUnit};
use arm::constants::{BATCH_AGGREGATION_PK, BATCH_AGGREGATION_VK, COMPLIANCE_PK, COMPLIANCE_VK};
use arm::delta_proof::DeltaWitness;
use arm::logic_instance::ExpirableBlob;
use arm::logic_instance::{AppData, LogicInstance};
use arm::logic_proof::{LogicVerifier, LogicVerifierInputsExt};
use arm::merkle_path::MerklePath;
use arm::nullifier_key::{NullifierKey, NullifierKeyExt};
use arm::proving_system::encode_seal;
use arm::proving_system::ProofType as LocalProofType;
use arm::resource::Resource;
use arm::transaction::{Delta, Transaction, TransactionExt};
use arm::utils::core_to_risc0_digest;
use arm::CoreDeltaWitness;
use arm::Digest;
use arm::MerklePathExt;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use heliax_ap_orchestrator_sdk::{
    AggregateProofResult, BaseProofResult, GpuAggregationProofPayload, GpuComplianceProofPayload,
    GpuLogicProofPayload, ProofPayload, ProofType as QueueProofType, QueueClient,
};
use k256::Scalar;
use risc0_zkvm::sha::{Digestible as _, Sha256 as _};
use risc0_zkvm::{InnerReceipt, MaybePruned, Receipt, ReceiptClaim};
use serde::Serialize;
use std::collections::BTreeSet;
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

/// Per-HTTP-request timeout for queue calls. Queue-side job timeouts (5–10 min)
/// are independent.
const QUEUE_REQUEST_TIMEOUT: Duration = Duration::from_secs(65);
/// How often to re-poll the queue while waiting for a job to finish.
const QUEUE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Build a queue client from `QUEUE_BASE_URL` + `QUEUE_AUTH_TOKEN`. Both are
/// required when the queue prover is selected (see `Prover`/`--prover`); the
/// local prover needs neither.
fn build_queue_client() -> Result<QueueClient> {
    let base_url = env::var("QUEUE_BASE_URL")
        .map_err(|_| anyhow!("QUEUE_BASE_URL must be set (workers queue endpoint)"))?;
    let auth_token = env::var("QUEUE_AUTH_TOKEN")
        .map_err(|_| anyhow!("QUEUE_AUTH_TOKEN must be set (workers queue bearer token)"))?;
    QueueClient::builder(&base_url)
        .timeout(QUEUE_REQUEST_TIMEOUT)
        .auth_token(&auth_token)
        .build()
        .map_err(|e| anyhow!("failed to build queue client: {e}"))
}

/// Submit one compliance witness to the queue and return the resulting
/// `ComplianceUnit`. Verifies the response was generated under the
/// `COMPLIANCE_VK` we sent so we can't accept a proof for a different
/// statement.
async fn queue_compliance_proof(
    client: &QueueClient,
    witness: &ComplianceWitness,
) -> Result<ComplianceUnit> {
    let witness_words = risc0_zkvm::serde::to_vec(witness)
        .map_err(|e| anyhow!("encode compliance witness: {e}"))?;
    let payload = GpuComplianceProofPayload(ProofPayload {
        witness: witness_words,
        proving_key: COMPLIANCE_PK.to_vec(),
        proof_type: QueueProofType::Succinct,
        verifying_key: COMPLIANCE_VK.as_bytes().to_vec(),
    });
    let result: BaseProofResult = client
        .submit_and_wait(payload, QUEUE_POLL_INTERVAL)
        .await
        .map_err(|e| anyhow!("queue compliance dispatch: {e}"))?;
    if result.verifying_key != COMPLIANCE_VK.as_bytes() {
        bail!("queue returned compliance proof for a different verifying key");
    }
    Ok(ComplianceUnit {
        proof: Some(result.receipt),
        instance: result.instance,
    })
}

/// Submit one logic-proof job (any logic guest ELF) and return `(receipt,
/// journal)` so the caller can plug into the existing `LogicVerifier` /
/// journal-bytes plumbing.
async fn queue_logic_proof<W: Serialize>(
    client: &QueueClient,
    proving_key: &[u8],
    verifying_key: &Digest,
    instance: &W,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let witness_words =
        risc0_zkvm::serde::to_vec(instance).map_err(|e| anyhow!("encode logic witness: {e}"))?;
    let payload = GpuLogicProofPayload(ProofPayload {
        witness: witness_words,
        proving_key: proving_key.to_vec(),
        proof_type: QueueProofType::Succinct,
        verifying_key: verifying_key.as_bytes().to_vec(),
    });
    let result: BaseProofResult = client
        .submit_and_wait(payload, QUEUE_POLL_INTERVAL)
        .await
        .map_err(|e| anyhow!("queue logic dispatch: {e}"))?;
    if result.verifying_key != verifying_key.as_bytes() {
        bail!("queue returned logic proof for a different verifying key");
    }
    Ok((result.receipt, result.instance))
}

/// Hand the transaction to the queue's GPU aggregation worker. Caller-supplied
/// `BATCH_AGGREGATION_PK` and `COMPLIANCE_VK` pin the worker to our circuit
/// images. Returns a transaction with `aggregation_proof` populated and the
/// individual base proofs erased.
async fn queue_aggregate_proof(client: &QueueClient, tx: Transaction) -> Result<Transaction> {
    let serialized = bincode::serialize(&tx).context("serialize tx for aggregation")?;
    let payload = GpuAggregationProofPayload {
        transaction: serialized,
        batch_aggregation_pk: Some(BATCH_AGGREGATION_PK.to_vec()),
        compliance_vk: Some(COMPLIANCE_VK.as_bytes().to_vec()),
    };
    let result: AggregateProofResult = client
        .submit_and_wait(payload, QUEUE_POLL_INTERVAL)
        .await
        .map_err(|e| anyhow!("queue aggregation dispatch: {e}"))?;
    bincode::deserialize(&result.transaction).context("deserialize aggregated tx")
}

/// Which prover backend generates the base and aggregation proofs.
///
/// `Local` runs risc0's CPU prover in-process (`default_prover()` with no
/// `BONSAI_API_URL` set, so it never dials out); the Groth16 aggregation step
/// still shells out to a container runtime (podman/docker) for the STARK ->
/// Groth16 wrapper, same as any local risc0 groth16 proof. `Queue` dispatches
/// to the AnomaPay workers queue instead.
enum Prover {
    Local,
    Queue(QueueClient),
}

/// Prove one compliance witness locally on CPU. Runs on tokio's blocking
/// thread pool since risc0 proving is synchronous, CPU-bound work.
async fn local_compliance_proof(witness: &ComplianceWitness) -> Result<ComplianceUnit> {
    let witness = witness.clone();
    tokio::task::spawn_blocking(move || {
        create_compliance_unit(&witness, LocalProofType::Succinct)
            .map_err(|e| anyhow!("local compliance proof: {e:?}"))
    })
    .await
    .context("local compliance proving task panicked")?
}

/// Prove one logic instance locally on CPU. Mirrors `queue_logic_proof`'s
/// `(proof, journal)` return shape.
async fn local_logic_proof<T>(proving_key: &'static [u8], instance: T) -> Result<(Vec<u8>, Vec<u8>)>
where
    T: Serialize + Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        arm::proving_system::prove(proving_key, &instance, LocalProofType::Succinct)
            .map_err(|e| anyhow!("local logic proof: {e:?}"))
    })
    .await
    .context("local logic proving task panicked")?
}

/// Aggregate a transaction's base proofs into a single Groth16 proof locally.
async fn local_aggregate_proof(mut tx: Transaction) -> Result<Transaction> {
    tokio::task::spawn_blocking(move || {
        tx.aggregate(LocalProofType::Groth16)
            .map_err(|e| anyhow!("local aggregate proof: {e:?}"))?;
        Ok(tx)
    })
    .await
    .context("local aggregation task panicked")?
}

/// Prove one compliance witness via the selected `Prover`.
async fn prove_compliance(prover: &Prover, witness: &ComplianceWitness) -> Result<ComplianceUnit> {
    match prover {
        Prover::Queue(client) => queue_compliance_proof(client, witness).await,
        Prover::Local => local_compliance_proof(witness).await,
    }
}

/// Prove one logic instance via the selected `Prover`.
async fn prove_logic<T>(
    prover: &Prover,
    proving_key: &'static [u8],
    verifying_key: &Digest,
    instance: T,
) -> Result<(Vec<u8>, Vec<u8>)>
where
    T: Serialize + Send + 'static,
{
    match prover {
        Prover::Queue(client) => {
            queue_logic_proof(client, proving_key, verifying_key, &instance).await
        }
        Prover::Local => local_logic_proof(proving_key, instance).await,
    }
}

/// Aggregate a transaction's base proofs via the selected `Prover`.
async fn aggregate_tx(prover: &Prover, tx: Transaction) -> Result<Transaction> {
    match prover {
        Prover::Queue(client) => queue_aggregate_proof(client, tx).await,
        Prover::Local => local_aggregate_proof(tx).await,
    }
}

/// `ComplianceUnit::instance` is journal bytes on the wire — parse, mutate
/// (via `f`), and re-encode in one place.
fn mutate_compliance_instance<R>(
    cu: &mut ComplianceUnit,
    f: impl FnOnce(&mut ComplianceInstance) -> R,
) -> Result<R> {
    let mut inst =
        ComplianceInstance::from_journal(&cu.instance).context("parse compliance instance")?;
    let r = f(&mut inst);
    cu.instance = inst
        .to_journal()
        .context("re-encode mutated compliance instance")?;
    Ok(r)
}

use block_time_forwarder::{RESULT_GT, RESULT_LT};
use solana_pa::external_calls::encode_external_call;
use solana_pa::types::{OutputMode, SolanaExternalCall};
use test_forwarder::{MODE_FAIL, MODE_SILENT};

#[derive(Serialize)]
struct Fixture {
    format: &'static str,
    aggregation_strategy: &'static str,
    aggregation_proof_type: &'static str,
    /// Groth16 verifier selector extracted from the proof's verifier_parameters.
    /// Format: "0x" + 4-byte hex (e.g., "0x73c457ba").
    selector: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarder_type: Option<&'static str>,
    tx_b64: String,
    tx_tampered_b64: String,
    consumed_nullifiers_b64: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    historical_roots_b64: Vec<String>,
}

const FIXTURE_FORMAT: &str = "arm-risc0:Transaction(bincode)";

enum Command {
    Generate(GenerateArgs),
    StripCalls {
        input: PathBuf,
        output: PathBuf,
    },
    Dump {
        input: PathBuf,
    },
    ImportBackendResult {
        input: PathBuf,
        output: PathBuf,
        root_account_dir: Option<PathBuf>,
        program_id: [u8; 32],
    },
    HistoricalRoot {
        batch_groth16_path: PathBuf,
        committer_out: PathBuf,
        consumer_out: PathBuf,
        prover_choice: Option<ProverChoice>,
        mock: bool,
    },
    Mockify {
        input: PathBuf,
        output: PathBuf,
    },
}

struct GenerateArgs {
    debug_assumptions: bool,
    forwarder_mode: ForwarderMode,
    nonce_seed: Option<u8>,
    multi_external_call: bool,
    error_variants_dir: Option<PathBuf>,
    out_path: PathBuf,
    prover_choice: Option<ProverChoice>,
    mock: bool,
}

/// Explicit `--prover` selection. `None` (the flag was not passed) resolves
/// to `Queue` if `QUEUE_BASE_URL` is set in the environment, `Local`
/// otherwise — see `resolve_prover`.
#[derive(Clone, Copy)]
enum ProverChoice {
    Local,
    Queue,
}

enum ForwarderMode {
    BlockTimeForwarder { output_mismatch: bool },
    TestForwarderFail,
    TestForwarderSilent,
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

/// Selector the localnet mock verifier is registered under in the synthetic
/// VerifierEntry preloaded at test-validator genesis (risc0 fake-receipt
/// convention; the real Groth16 selector is 0x73c457ba).
const MOCK_SELECTOR: [u8; 4] = [0xff; 4];

/// Claim digest a mock seal must carry, derived from the transaction alone:
/// the digest of the batch-aggregation receipt claim over the aggregation
/// journal the on-chain PA independently recomputes at settle time.
fn mock_claim_digest(tx: &Transaction) -> Result<risc0_zkvm::sha::Digest> {
    let journal = tx
        .construct_aggregation_instance()
        .map_err(|e| anyhow!("construct aggregation instance: {e:?}"))?;
    Ok(compute_expected_claim_digest(
        &journal,
        &BATCH_AGGREGATION_VK,
    ))
}

/// Build the 260-byte router `Seal` the mock verifier accepts: selector
/// 0xffffffff, claim digest in pi_c[..32], zeros elsewhere. The digest rides
/// in pi_c because the PA negates pi_a before the router CPI.
fn mock_seal_bytes(claim: risc0_zkvm::sha::Digest) -> Result<Vec<u8>> {
    let mut pi_c = [0u8; 64];
    pi_c[..32].copy_from_slice(claim.as_bytes());
    let seal = Seal {
        selector: MOCK_SELECTOR,
        proof: groth_16_verifier::Proof {
            pi_a: [0u8; 64],
            pi_b: [0u8; 128],
            pi_c,
        },
    };
    let bytes = seal.try_to_vec().context("serialize mock Seal")?;
    if bytes.len() != 260 {
        bail!("mock seal must be exactly 260 bytes, got {}", bytes.len());
    }
    Ok(bytes)
}

/// Encode a dev-mode (Fake) aggregation receipt as a mock router seal,
/// cross-checking the receipt's claim digest against the one derived from
/// the transaction alone so any journal-derivation drift fails loudly.
fn encode_mock_seal(agg_proof_bytes: &[u8], tx: &Transaction) -> Result<Vec<u8>> {
    let inner: InnerReceipt =
        bincode::deserialize(agg_proof_bytes).context("decode aggregation receipt")?;
    let InnerReceipt::Fake(fake) = inner else {
        bail!("expected a dev-mode (Fake) aggregation receipt");
    };
    let receipt_claim = fake.claim.digest();
    let derived_claim = mock_claim_digest(tx)?;
    if receipt_claim != derived_claim {
        bail!(
            "dev-mode receipt claim digest ({receipt_claim}) != transaction-derived claim \
             digest ({derived_claim}) — the aggregation journal derivation drifted"
        );
    }
    mock_seal_bytes(derived_claim)
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

    let (old_created_commitment, new_created_commitment) =
        mutate_compliance_instance(cu, |instance| {
            let old = instance.created_commitment;
            let mut new_bytes = [0u8; 32];
            new_bytes.copy_from_slice(old.as_bytes());
            new_bytes[0] ^= 1;
            let new = Digest::from_bytes(new_bytes);
            instance.created_commitment = new;
            (old, new)
        })?;

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
    let program_id = decode_base58_32("3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf")?;

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
        num_accounts: 2,
    }))
}

fn test_forwarder_fail_payload_blob() -> Result<ExpirableBlob> {
    // expected_output must be non-empty: Solana's runtime reports no return-data
    // record both for an explicit empty return and for no return at all, so
    // decode_external_call rejects an empty expected_output before the call is
    // ever attempted. This test needs the call to actually reach the CPI so the
    // test-forwarder's IntentionalFailure can propagate, so we pin a non-empty
    // expected value; it is never compared because the CPI itself fails first.
    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder_program_id()?,
        instruction_data: vec![MODE_FAIL],
        expected_output: vec![0x2a],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    }))
}

fn test_forwarder_silent_payload_blob() -> Result<ExpirableBlob> {
    // expected_output must be non-empty: Solana's runtime reports no return-data
    // record both for an explicit empty return and for no return at all, so
    // decode_external_call rejects an empty expected_output before the call is
    // ever attempted. Pinning a non-empty expected value here makes the silent
    // forwarder path a genuine output mismatch (expected [0x2a], got nothing)
    // rather than conflating "expected empty" with "returned nothing".
    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder_program_id()?,
        instruction_data: vec![MODE_SILENT],
        expected_output: vec![0x2a],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    }))
}

async fn generate_test_transaction_with_external_payload(
    prover: &Prover,
    forwarder_mode: ForwarderMode,
    nonce_seed: Option<u8>,
    multi_external_call: bool,
) -> Result<Transaction> {
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
    let compliance_receipt = prove_compliance(prover, &compliance_witness)
        .await
        .context("prove compliance")?;

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

    let (consumed_proof, consumed_journal) = prove_logic(
        prover,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        consumed_instance,
    )
    .await
    .context("prove consumed passthrough logic")?;
    let (created_proof, created_journal) = prove_logic(
        prover,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        created_instance,
    )
    .await
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
    proof_type: &'static str,
    nullifiers_b64: &[String],
    out_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create error variants dir {}", out_dir.display()))?;

    let write_variant = |file_name: &str, variant_tx: &Transaction| -> Result<()> {
        let tx_bytes = bincode::serialize(variant_tx)
            .with_context(|| format!("serialize variant tx for {file_name}"))?;
        let fixture = Fixture {
            format: FIXTURE_FORMAT,
            aggregation_strategy: "batch",
            aggregation_proof_type: proof_type,
            selector: selector.to_owned(),
            forwarder_type: None,
            tx_b64: BASE64.encode(tx_bytes),
            tx_tampered_b64: String::new(),
            consumed_nullifiers_b64: nullifiers_b64.to_vec(),
            historical_roots_b64: Vec::new(),
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
        mutate_compliance_instance(cu, |instance| {
            instance.consumed_commitment_tree_root = Digest::from_bytes([1u8; 32]);
        })?;
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

fn compliance_instances(tx: &Transaction) -> Result<Vec<ComplianceInstance>> {
    tx.actions
        .iter()
        .flat_map(|action| action.compliance_units.iter())
        .map(|cu| {
            ComplianceInstance::from_journal(&cu.instance)
                .context("decode compliance instance from journal bytes")
        })
        .collect()
}

fn consumed_nullifiers_b64(tx: &Transaction) -> Result<Vec<String>> {
    Ok(compliance_instances(tx)?
        .into_iter()
        .map(|instance| BASE64.encode(instance.consumed_nullifier.as_bytes()))
        .collect())
}

fn historical_roots(tx: &Transaction) -> Result<Vec<[u8; 32]>> {
    let initial = initial_root();
    let mut roots = BTreeSet::new();
    for instance in compliance_instances(tx)? {
        let root = instance.consumed_commitment_tree_root;
        if root != initial {
            roots.insert(root.to_bytes());
        }
    }
    Ok(roots.into_iter().collect())
}

#[derive(Serialize)]
struct ValidatorAccountFixture {
    pubkey: String,
    account: ValidatorAccount,
}

#[derive(Serialize)]
struct ValidatorAccount {
    lamports: u64,
    data: [String; 2],
    owner: String,
    executable: bool,
    #[serde(rename = "rentEpoch")]
    rent_epoch: u64,
    space: u64,
}

fn write_root_marker_accounts(
    roots: &[[u8; 32]],
    program_id: [u8; 32],
    out_dir: &Path,
) -> Result<()> {
    fs::create_dir_all(out_dir)
        .with_context(|| format!("create root marker account dir {}", out_dir.display()))?;

    let pa_program_id = Pubkey::new_from_array(program_id);
    let (pa_state, _) =
        Pubkey::find_program_address(&[solana_pa::state::PA_STATE_SEED], &pa_program_id);

    for root in roots {
        let (marker, _) = solana_pa::root::derive_root_pda(&pa_program_id, &pa_state, root);
        let account = ValidatorAccountFixture {
            pubkey: marker.to_string(),
            account: ValidatorAccount {
                lamports: 1_000_000,
                data: [String::new(), "base64".to_string()],
                owner: pa_program_id.to_string(),
                executable: false,
                rent_epoch: u64::MAX,
                space: 0,
            },
        };
        let out_path = out_dir.join(format!("root-marker-{marker}.json"));
        fs::write(&out_path, serde_json::to_vec_pretty(&account)?)
            .with_context(|| format!("write root marker account {}", out_path.display()))?;
    }

    Ok(())
}

fn import_backend_result_fixture(
    input: &Path,
    output: &Path,
    root_account_dir: Option<&Path>,
    program_id: [u8; 32],
) -> Result<()> {
    let raw = fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let mut tx: Transaction = serde_json::from_slice(&raw)
        .with_context(|| format!("decode backend Transaction JSON {}", input.display()))?;

    tx.verify_aggregation()
        .context("verify imported backend aggregation proof")?;

    let roots = historical_roots(&tx).context("extract imported historical roots")?;
    let historical_roots_b64 = roots.iter().map(|root| BASE64.encode(root)).collect();
    let consumed_nullifiers_b64 =
        consumed_nullifiers_b64(&tx).context("extract imported nullifiers")?;

    let agg_proof_bytes = tx
        .aggregation_proof
        .as_ref()
        .ok_or_else(|| anyhow!("imported transaction is missing aggregation_proof"))?;
    tx.aggregation_proof = Some(encode_seal(agg_proof_bytes).context("encode imported seal")?);

    let selector = extract_selector(&tx).context("extract imported selector")?;
    let tx_bytes = bincode::serialize(&tx).context("serialize imported transaction")?;

    let mut tx_tampered = tx.clone();
    mutate_created_commitment_keep_structure(&mut tx_tampered)
        .context("tamper imported transaction")?;
    let tx_tampered_bytes =
        bincode::serialize(&tx_tampered).context("serialize tampered imported transaction")?;

    let fixture = Fixture {
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: "groth16",
        selector,
        forwarder_type: Some("anomapay_transfer"),
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tx_tampered_bytes),
        consumed_nullifiers_b64,
        historical_roots_b64,
    };

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
    }
    fs::write(output, serde_json::to_vec_pretty(&fixture)?)
        .with_context(|| format!("write imported fixture {}", output.display()))?;

    if let Some(root_account_dir) = root_account_dir {
        write_root_marker_accounts(&roots, program_id, root_account_dir)?;
    }

    Ok(())
}

/// Seal-encode the aggregation proof, serialize the transaction (and a
/// tampered clone), extract nullifiers/selector/historical roots, and write
/// the resulting `Fixture` JSON. Shared by the default `Generate` path and
/// the `historical-root` path so both fixtures follow the exact same
/// on-disk convention.
fn finalize_and_write_fixture(
    tx: &mut Transaction,
    out_path: &Path,
    forwarder_type: Option<&'static str>,
) -> Result<Fixture> {
    // The receipt type decides the seal encoding: dev-mode (Fake) receipts
    // become mock seals for the localnet mock verifier, real Groth16
    // receipts go through arm's canonical seal encoding. The fixture is
    // labeled accordingly (aggregation_proof_type, selector).
    let proof_type = timed_phase("encode_seal", || {
        let agg_proof_bytes = tx.aggregation_proof.clone().unwrap();
        let inner: InnerReceipt =
            bincode::deserialize(&agg_proof_bytes).context("decode aggregation receipt")?;
        Ok(if matches!(inner, InnerReceipt::Fake(_)) {
            eprintln!("  dev-mode receipt -> mock seal (selector 0xffffffff)");
            tx.aggregation_proof = Some(encode_mock_seal(&agg_proof_bytes, tx)?);
            "mock"
        } else {
            tx.aggregation_proof = Some(encode_seal(&agg_proof_bytes).context("encode seal")?);
            "groth16"
        })
    })?;

    let tx_bytes = timed_phase("serialize_tx", || {
        let bytes = bincode::serialize(&*tx).context("serialize tx")?;
        eprintln!("  {} bytes", bytes.len());
        Ok(bytes)
    })?;

    let consumed_nullifiers_b64 =
        timed_phase("extract_nullifiers", || consumed_nullifiers_b64(tx))?;

    let historical_roots_b64 = timed_phase("extract_historical_roots", || {
        Ok(historical_roots(tx)?
            .iter()
            .map(|root| BASE64.encode(root))
            .collect::<Vec<_>>())
    })?;

    let (tx_tampered_bytes, selector) = timed_phase("tamper_and_extract_selector", || {
        let mut tx_tampered = tx.clone();
        mutate_created_commitment_keep_structure(&mut tx_tampered)?;
        let tampered_bytes = bincode::serialize(&tx_tampered).context("serialize tampered tx")?;
        eprintln!("  tampered: {} bytes", tampered_bytes.len());

        let sel = extract_selector(tx).context("extract selector from proof")?;
        eprintln!("  selector: {sel}");
        Ok((tampered_bytes, sel))
    })?;

    let fixture = Fixture {
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: proof_type,
        selector,
        forwarder_type,
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tx_tampered_bytes),
        consumed_nullifiers_b64,
        historical_roots_b64,
    };

    timed_phase("write_fixture", || {
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create dir {parent:?}"))?;
        }
        fs::write(out_path, serde_json::to_vec_pretty(&fixture)?)
            .with_context(|| format!("write fixture to {}", out_path.display()))?;
        Ok(())
    })?;

    eprintln!("wrote fixture: {}", out_path.display());

    Ok(fixture)
}

/// Nonce byte reserved for the historical-root committer/consumer pair.
/// Existing fixtures use 0 (default), 2 (output-mismatch), and 3-7
/// (`--nonce-seed`, see v2/v3/multi-call/forwarder-fail/forwarder-silent), so
/// 8 is unused and avoids a `DuplicateNullifier` collision.
const HISTORICAL_ROOT_NONCE_BYTE: u8 = 8;

/// Read an existing single-action, single-compliance-unit fixture and return
/// the digest of its created resource's commitment -- the leaf that
/// settlement inserted into the on-chain commitment tree at index 0. Used to
/// reconstruct, off-chain, the exact tree state the historical-root
/// committer transaction lands in as leaf index 1.
fn read_sole_created_commitment(path: &Path) -> Result<Digest> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw).context("parsing fixture JSON")?;
    let tx_b64 = value["tx_b64"]
        .as_str()
        .ok_or_else(|| anyhow!("missing tx_b64 field in {}", path.display()))?;
    let tx_bytes = BASE64.decode(tx_b64).context("decoding tx_b64")?;
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")?;

    if tx.actions.len() != 1 || tx.actions[0].compliance_units.len() != 1 {
        bail!(
            "{} must have exactly one action with one compliance unit to serve as \
             the known single-leaf tree base for historical-root fixture generation \
             (found {} action(s))",
            path.display(),
            tx.actions.len()
        );
    }
    let instance = ComplianceInstance::from_journal(&tx.actions[0].compliance_units[0].instance)
        .context("decode compliance instance")?;
    Ok(instance.created_commitment)
}

/// Build the (unproven) compliance witness and matching created resource for
/// the historical-root *committer* transaction: consumes a fresh ephemeral
/// resource as usual, but its created resource is genuinely non-ephemeral
/// (`is_ephemeral: false`), so a later transaction can consume it through a
/// real Merkle-inclusion proof rather than the ephemeral-root shortcut.
/// Returns the witness plus the created resource and the nullifier key that
/// unlocks it, both needed to build the consumer transaction afterward.
fn build_historical_root_committer_witness() -> Result<(ComplianceWitness, Resource, NullifierKey)>
{
    let passthrough_vk = Digest(PASSTHROUGH_LOGIC_GUEST_ID);
    let nf_key = NullifierKey::default();
    let nf_key_cm = nf_key.commit();

    let mut consumed_resource = Resource {
        logic_ref: passthrough_vk,
        nk_commitment: nf_key_cm,
        quantity: 1,
        is_ephemeral: true,
        ..Default::default()
    };
    consumed_resource.nonce = [[HISTORICAL_ROOT_NONCE_BYTE; 16], [0u8; 16]]
        .concat()
        .try_into()
        .unwrap();
    let consumed_nf = consumed_resource
        .nullifier(&nf_key)
        .context("compute committer consumed nullifier")?;

    let mut created_resource = consumed_resource;
    created_resource.set_nonce(consumed_nf);
    created_resource.is_ephemeral = false;

    let compliance_witness = ComplianceWitness {
        consumed_resource,
        created_resource,
        merkle_path: MerklePath::empty(),
        rcv: Scalar::ONE.to_bytes().to_vec(),
        nf_key: nf_key.clone(),
        ephemeral_root: initial_root(),
    };

    Ok((compliance_witness, created_resource, nf_key))
}

/// Build the (unproven) compliance witness and matching created resource for
/// the historical-root *consumer* transaction: genuinely consumes
/// `committed_resource` (is_ephemeral: false) via `merkle_path`, which must
/// reconstruct the real on-chain root the committer's settlement produced.
fn build_historical_root_consumer_witness(
    committed_resource: Resource,
    committer_nf_key: NullifierKey,
    merkle_path: MerklePath,
) -> Result<(ComplianceWitness, Resource)> {
    let passthrough_vk = Digest(PASSTHROUGH_LOGIC_GUEST_ID);

    let consumed_nf = committed_resource
        .nullifier(&committer_nf_key)
        .context("compute consumer's consumed nullifier")?;

    let output_nf_key = NullifierKey::default();
    let mut created_resource = Resource {
        logic_ref: passthrough_vk,
        nk_commitment: output_nf_key.commit(),
        quantity: 1,
        is_ephemeral: true,
        ..Default::default()
    };
    created_resource.set_nonce(consumed_nf);

    let compliance_witness = ComplianceWitness::from_resources_with_path(
        committed_resource,
        committer_nf_key,
        merkle_path,
        created_resource,
    );

    Ok((compliance_witness, created_resource))
}

/// Prove `compliance_witness` and wrap it (plus a matching pair of
/// passthrough logic proofs) into a balanced, delta-proved `Transaction`.
/// No external call: this is used only by the historical-root committer and
/// consumer, which test root retention, not the forwarder CPI path.
async fn prove_historical_root_transaction(
    prover: &Prover,
    compliance_witness: ComplianceWitness,
    created_resource: Resource,
) -> Result<Transaction> {
    let passthrough_vk = Digest(PASSTHROUGH_LOGIC_GUEST_ID);

    let consumed_cm = compliance_witness.consumed_resource.commitment();
    let consumed_nf = compliance_witness
        .consumed_resource
        .nullifier_from_commitment(&compliance_witness.nf_key, &consumed_cm)
        .context("compute consumed nullifier")?;
    let created_cm = created_resource.commitment();
    let rcv = compliance_witness.rcv.clone();

    let compliance_receipt = prove_compliance(prover, &compliance_witness)
        .await
        .context("prove compliance")?;

    let tags = vec![consumed_nf, created_cm];
    let action_tree = MerkleTree::from(tags);
    let root = action_tree.root().context("compute action tree root")?;

    let consumed_instance = LogicInstance {
        tag: consumed_nf,
        is_consumed: true,
        root,
        app_data: AppData::default(),
    };
    let created_instance = LogicInstance {
        tag: created_cm,
        is_consumed: false,
        root,
        app_data: AppData::default(),
    };

    let (consumed_proof, consumed_journal) = prove_logic(
        prover,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        consumed_instance,
    )
    .await
    .context("prove consumed passthrough logic")?;
    let (created_proof, created_journal) = prove_logic(
        prover,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        created_instance,
    )
    .await
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

    let delta_witness = DeltaWitness::from_bytes_vec(&[rcv]).context("build delta witness")?;

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

/// Generate the historical-root committer and consumer fixtures.
///
/// The committer transaction's created resource is genuinely non-ephemeral,
/// so it is inserted into the on-chain commitment tree the same way as any
/// other created resource, but it can later be *consumed* through a real
/// Merkle-inclusion proof (unlike every other existing fixture's created
/// resource, which is `is_ephemeral: true` and can therefore only ever be
/// re-admitted through the unconstrained `ephemeral_root` shortcut).
///
/// The committer is expected to settle immediately after `batch_groth16.json`
/// (leaf index 0) and nothing else, landing at leaf index 1: the on-chain
/// tree grows from depth 1 to depth 2, and the resulting root is
/// `hash_two(hash_two(batch_groth16_leaf, committer_leaf), ZEROS[1])`. That
/// exact computation is replicated here using the PA's own on-chain merkle
/// constants (`solana_pa::merkle`), and independently cross-checked against
/// `MerklePathExt::root()` (arm's own hash) before any proof is generated, so
/// a divergence between the two hash implementations fails loudly instead of
/// producing a fixture that can never settle.
async fn generate_historical_root_fixtures(
    batch_groth16_path: &Path,
    committer_out: &Path,
    consumer_out: &Path,
    prover_choice: Option<ProverChoice>,
) -> Result<()> {
    let prover = resolve_prover(prover_choice)?;
    match &prover {
        Prover::Local => eprintln!("prover: local (CPU risc0 prover)"),
        Prover::Queue(_) => eprintln!(
            "prover: queue ({})",
            env::var("QUEUE_BASE_URL").unwrap_or_default()
        ),
    }

    let batch_groth16_leaf = read_sole_created_commitment(batch_groth16_path)
        .context("read batch_groth16.json's committed leaf")?;

    eprintln!("phase: generate historical-root committer transaction");
    let commit_start = Instant::now();
    let (committer_witness, committed_resource, committer_nf_key) =
        build_historical_root_committer_witness()?;
    let committer_created_resource = committer_witness.created_resource;
    let mut committer_tx =
        prove_historical_root_transaction(&prover, committer_witness, committer_created_resource)
            .await
            .context("build committer transaction")?;
    eprintln!(
        "phase done: committer transaction ({})",
        fmt_duration(commit_start.elapsed())
    );

    eprintln!("phase: aggregate committer transaction (batch, groth16)");
    let agg_start = Instant::now();
    committer_tx = aggregate_tx(&prover, committer_tx)
        .await
        .context("aggregate committer tx")?;
    eprintln!(
        "phase done: aggregate committer ({})",
        fmt_duration(agg_start.elapsed())
    );
    committer_tx
        .verify_aggregation()
        .context("verify committer aggregated proof")?;

    finalize_and_write_fixture(
        &mut committer_tx,
        committer_out,
        Some("historical_root_committer"),
    )
    .context("write committer fixture")?;

    // Independently reconstruct the root the on-chain program will produce
    // once batch_groth16.json (leaf 0) and this committer (leaf 1) have both
    // settled, using the PA's own on-chain constants/hash directly -- not
    // ARM's hash -- so the two implementations are cross-checked rather than
    // assumed equivalent.
    let committed_cm = committed_resource.commitment();
    let expected_root = solana_pa::merkle::hash_two(
        &solana_pa::merkle::hash_two(&batch_groth16_leaf, &committed_cm),
        &solana_pa::merkle::ZEROS[1],
    );

    let merkle_path = MerklePath::from_path(&[
        (batch_groth16_leaf, true),
        (solana_pa::merkle::ZEROS[1], false),
    ]);
    let path_root = merkle_path.root(&committed_cm);
    if path_root != expected_root {
        bail!(
            "historical-root merkle path does not reconstruct the on-chain root: \
             ARM MerklePath::root()={} vs PA on-chain hash_two()={} -- the two hash \
             implementations must match before any proof is generated",
            hex::encode(path_root.to_bytes()),
            hex::encode(expected_root.to_bytes())
        );
    }
    eprintln!(
        "verified: MerklePath::root() matches the PA's own hash_two computation ({})",
        hex::encode(expected_root.to_bytes())
    );

    eprintln!("phase: generate historical-root consumer transaction");
    let consume_start = Instant::now();
    let (consumer_witness, consumer_created_resource) =
        build_historical_root_consumer_witness(committed_resource, committer_nf_key, merkle_path)
            .context("build consumer witness")?;
    let mut consumer_tx =
        prove_historical_root_transaction(&prover, consumer_witness, consumer_created_resource)
            .await
            .context("build consumer transaction")?;
    eprintln!(
        "phase done: consumer transaction ({})",
        fmt_duration(consume_start.elapsed())
    );

    eprintln!("phase: aggregate consumer transaction (batch, groth16)");
    let agg_start = Instant::now();
    consumer_tx = aggregate_tx(&prover, consumer_tx)
        .await
        .context("aggregate consumer tx")?;
    eprintln!(
        "phase done: aggregate consumer ({})",
        fmt_duration(agg_start.elapsed())
    );
    consumer_tx
        .verify_aggregation()
        .context("verify consumer aggregated proof")?;

    // The whole point of this fixture: the consumed root must be a genuine,
    // non-padding historical root. If it were PADDING_LEAF, is_root_valid
    // would accept it unconditionally before the marker lookup ever runs,
    // exactly the coverage gap this fixture exists to close.
    let consumer_instance = compliance_instances(&consumer_tx)
        .context("decode consumer compliance instances")?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("consumer tx has no compliance instances"))?;
    if consumer_instance.consumed_commitment_tree_root == initial_root() {
        bail!(
            "consumer's consumed_commitment_tree_root is PADDING_LEAF -- this fixture \
             would prove nothing about historical root retention"
        );
    }
    if consumer_instance.consumed_commitment_tree_root.to_bytes() != expected_root.to_bytes() {
        bail!(
            "consumer's consumed_commitment_tree_root ({}) does not match the expected \
             historical root ({})",
            hex::encode(consumer_instance.consumed_commitment_tree_root.to_bytes()),
            hex::encode(expected_root.to_bytes())
        );
    }
    eprintln!(
        "confirmed: consumer's consumed_commitment_tree_root = {} (non-padding, matches the \
         committer's post-settlement root)",
        hex::encode(consumer_instance.consumed_commitment_tree_root.to_bytes())
    );

    finalize_and_write_fixture(
        &mut consumer_tx,
        consumer_out,
        Some("historical_root_consumer"),
    )
    .context("write consumer fixture")?;

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

fn strip_calls_from_fixture(input: &Path, output: &Path) -> Result<()> {
    let fixture_str =
        fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;

    #[derive(serde::Deserialize, Serialize)]
    struct RawFixture {
        tx_b64: String,
        #[serde(flatten)]
        rest: serde_json::Map<String, serde_json::Value>,
    }

    let mut fixture: RawFixture =
        serde_json::from_str(&fixture_str).context("parsing fixture JSON")?;
    let tx_bytes = BASE64.decode(&fixture.tx_b64).context("decoding tx_b64")?;
    let mut tx: Transaction =
        bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")?;

    let mut stripped = 0usize;
    for action in &mut tx.actions {
        for lvi in &mut action.logic_verifier_inputs {
            let count = lvi.app_data.external_payload.len();
            if count > 0 {
                eprintln!(
                    "  Stripping {} external_payload blob(s) from LVI tag {:?}",
                    count,
                    &lvi.tag.to_bytes()[..4]
                );
                lvi.app_data.external_payload.clear();
                stripped += count;
            }
        }
    }
    eprintln!("Stripped {} external call(s) total", stripped);

    let modified_bytes = bincode::serialize(&tx).context("re-serializing Transaction")?;
    fixture.tx_b64 = BASE64.encode(&modified_bytes);
    fixture.rest.remove("forwarder_type");

    let output_str = serde_json::to_string_pretty(&fixture).context("serializing fixture")?;
    fs::write(output, output_str).with_context(|| format!("writing {}", output.display()))?;
    eprintln!("Wrote fixture to {}", output.display());
    Ok(())
}

/// Convert an existing (real-proof) fixture into its mock twin: replace the
/// aggregation seal with a mock seal derived from the transaction alone and
/// relabel selector/proof type. Everything else about the transaction stays
/// byte-identical, which the recomputed nullifier/root cross-checks enforce.
/// Exists for imported fixtures whose proving inputs are not in this repo
/// (the dev-mode pipeline cannot regenerate them).
fn mockify_fixture(input: &Path, output: &Path) -> Result<()> {
    let fixture_str =
        fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let mut fixture: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&fixture_str).context("parsing fixture JSON")?;

    let tx_b64 = fixture["tx_b64"]
        .as_str()
        .ok_or_else(|| anyhow!("missing tx_b64 field"))?;
    let tx_bytes = BASE64.decode(tx_b64).context("decoding tx_b64")?;
    let mut tx: Transaction =
        bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")?;

    if tx.aggregation_proof.is_none() {
        bail!("fixture transaction has no aggregation_proof to replace");
    }
    tx.aggregation_proof = Some(mock_seal_bytes(mock_claim_digest(&tx)?)?);

    // The mock twin must change nothing but the seal: recompute the derived
    // fields and require them to match the input fixture exactly.
    let nullifiers = consumed_nullifiers_b64(&tx)?;
    let existing_nullifiers: Vec<String> =
        serde_json::from_value(fixture["consumed_nullifiers_b64"].clone())
            .context("parsing consumed_nullifiers_b64")?;
    if nullifiers != existing_nullifiers {
        bail!("recomputed nullifiers differ from the input fixture's — refusing to write");
    }
    let roots: Vec<String> = historical_roots(&tx)?
        .iter()
        .map(|root| BASE64.encode(root))
        .collect();
    let existing_roots: Vec<String> = match fixture.get("historical_roots_b64") {
        Some(value) => {
            serde_json::from_value(value.clone()).context("parsing historical_roots_b64")?
        }
        None => Vec::new(),
    };
    if roots != existing_roots {
        bail!("recomputed historical roots differ from the input fixture's — refusing to write");
    }

    let mut tx_tampered = tx.clone();
    mutate_created_commitment_keep_structure(&mut tx_tampered)?;

    fixture.insert(
        "tx_b64".into(),
        BASE64.encode(bincode::serialize(&tx)?).into(),
    );
    fixture.insert(
        "tx_tampered_b64".into(),
        BASE64.encode(bincode::serialize(&tx_tampered)?).into(),
    );
    fixture.insert("selector".into(), extract_selector(&tx)?.into());
    fixture.insert("aggregation_proof_type".into(), "mock".into());

    fs::write(output, serde_json::to_string_pretty(&fixture)?)
        .with_context(|| format!("writing {}", output.display()))?;
    eprintln!("wrote mock fixture: {}", output.display());
    Ok(())
}

fn dump_fixture(input: &Path) -> Result<()> {
    let fixture_str =
        fs::read_to_string(input).with_context(|| format!("reading {}", input.display()))?;
    let raw: serde_json::Value =
        serde_json::from_str(&fixture_str).context("parsing fixture JSON")?;
    let tx_b64 = raw["tx_b64"]
        .as_str()
        .ok_or_else(|| anyhow!("missing tx_b64 field"))?;
    let tx_bytes = BASE64.decode(tx_b64).context("decoding tx_b64")?;
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")?;

    eprintln!("Transaction:");
    eprintln!("  actions: {}", tx.actions.len());
    for (ai, action) in tx.actions.iter().enumerate() {
        eprintln!("  Action {}:", ai);
        eprintln!("    compliance_units: {}", action.compliance_units.len());
        for (ci, cu) in action.compliance_units.iter().enumerate() {
            let instance = ComplianceInstance::from_journal(&cu.instance)
                .context("decode compliance instance for dump")?;
            let nf = instance.consumed_nullifier.to_bytes();
            let cm = instance.created_commitment.to_bytes();
            eprintln!(
                "    CU {}: nullifier={:02x}{:02x}..., commitment={:02x}{:02x}...",
                ci, nf[0], nf[1], cm[0], cm[1]
            );
        }
        eprintln!(
            "    logic_verifier_inputs: {}",
            action.logic_verifier_inputs.len()
        );
        for (li, lvi) in action.logic_verifier_inputs.iter().enumerate() {
            let tag = lvi.tag.to_bytes();
            let ext = lvi.app_data.external_payload.len();
            eprintln!(
                "    LVI {}: tag={:02x}{:02x}..., vk={:02x}{:02x}..., external_payload={}",
                li,
                tag[0],
                tag[1],
                lvi.verifying_key.to_bytes()[0],
                lvi.verifying_key.to_bytes()[1],
                ext
            );
        }
    }
    eprintln!(
        "  delta_proof: {:?}",
        std::mem::discriminant(&tx.delta_proof)
    );
    eprintln!(
        "  aggregation_proof: {} bytes",
        tx.aggregation_proof.as_ref().map_or(0, |p| p.len())
    );
    Ok(())
}

fn print_usage() {
    eprintln!(
        "Usage:\n  fixture-gen [OPTIONS] [OUT_PATH]         Generate a fixture (default)\n  fixture-gen import-backend-result --program-id PROGRAM_ID_B58 [--root-account-dir DIR] <IN_JSON> <OUT_JSON>\n  fixture-gen strip-calls <IN> <OUT>       Remove external calls from a fixture\n  fixture-gen dump <IN>                    Print transaction structure\n  fixture-gen mockify <IN> <OUT>           Convert an existing fixture into its mock twin\n  fixture-gen historical-root <batch_groth16.json> <committer_out.json> <consumer_out.json> [--prover local|queue] [--mock]\n                                            Generate the historical-root committer/consumer fixture pair\n\nGenerate options:\n  --debug-assumptions      Print claim digests for composition debugging\n  --output-mismatch        Wrong expected_output for ExternalCallOutputMismatch test\n  --forwarder-fail         Test-forwarder with failing instruction\n  --forwarder-silent       Test-forwarder with no return data\n  --nonce-seed N           Override deterministic nonce byte for nullifier derivation\n  --multi-external-call    Append a second block-time-forwarder external call blob\n  --error-variants DIR     Write wrong_root/no_aggregation/garbage_proof variants\n  --mock                   Dev-mode executor instead of proving (seconds, no GPU or\n                           podman proving step); emits a mock seal (selector 0xffffffff)\n                           only the localnet mock verifier accepts\n  --prover <local|queue>   Select the prover backend (default: queue if QUEUE_BASE_URL\n                           is set, local otherwise). local runs risc0's CPU prover\n                           in-process; queue dispatches to the AnomaPay workers queue\n                           via QUEUE_BASE_URL/QUEUE_AUTH_TOKEN.\n\nNotes:\n  - At most one of --output-mismatch, --forwarder-fail, --forwarder-silent.\n  - import-backend-result converts backend Transaction JSON into the on-chain TxData bincode fixture.\n  - With --prover queue (or no --prover and QUEUE_BASE_URL set), QUEUE_BASE_URL and\n    QUEUE_AUTH_TOKEN must be set; proofs are dispatched to the workers queue.\n  - With --prover local (or no --prover and QUEUE_BASE_URL unset), proofs run on the\n    local CPU risc0 prover; the Groth16 aggregation step needs a container runtime\n    (podman/docker) for the STARK -> Groth16 wrapper.\n  - --error-variants writes to DIR from the final aggregated tx.\n  - mockify replaces only the aggregation seal of an existing fixture; use it for\n    imported fixtures whose proving inputs are not in this repo.\n"
    );
}

fn parse_import_backend_result_args(args: impl Iterator<Item = String>) -> Result<Command> {
    let mut args = args;
    let mut root_account_dir: Option<PathBuf> = None;
    let mut program_id_b58: Option<String> = None;
    let mut positionals: Vec<PathBuf> = Vec::new();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root-account-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--root-account-dir requires a value"))?;
                root_account_dir = Some(PathBuf::from(value));
            }
            "--program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--program-id requires a value"))?;
                program_id_b58 = Some(value);
            }
            _ if arg.starts_with("--root-account-dir=") => {
                let value = arg
                    .split_once('=')
                    .map(|(_, value)| value)
                    .ok_or_else(|| anyhow!("--root-account-dir requires a value"))?;
                root_account_dir = Some(PathBuf::from(value));
            }
            _ if arg.starts_with("--program-id=") => {
                let value = arg
                    .split_once('=')
                    .map(|(_, value)| value)
                    .ok_or_else(|| anyhow!("--program-id requires a value"))?;
                program_id_b58 = Some(value.to_string());
            }
            _ if arg.starts_with('-') => {
                return Err(anyhow!("unknown flag in import-backend-result mode: {arg}"));
            }
            _ => positionals.push(PathBuf::from(arg)),
        }
    }

    if positionals.len() != 2 {
        return Err(anyhow!(
            "Usage: fixture-gen import-backend-result --program-id PROGRAM_ID_B58 [--root-account-dir DIR] <IN_JSON> <OUT_JSON>"
        ));
    }
    let program_id_b58 = program_id_b58
        .ok_or_else(|| anyhow!("import-backend-result requires --program-id PROGRAM_ID_B58"))?;
    let program_id = decode_base58_32(&program_id_b58).context("invalid --program-id")?;

    Ok(Command::ImportBackendResult {
        input: positionals.remove(0),
        output: positionals.remove(0),
        root_account_dir,
        program_id,
    })
}

fn parse_historical_root_args(args: impl Iterator<Item = String>) -> Result<Command> {
    let mut positionals: Vec<PathBuf> = Vec::new();
    let mut prover_choice: Option<ProverChoice> = None;
    let mut mock = false;
    let mut args = args;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mock" => {
                mock = true;
            }
            "--prover" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--prover requires a value"))?;
                prover_choice = Some(match value.as_str() {
                    "local" => ProverChoice::Local,
                    "queue" => ProverChoice::Queue,
                    _ => {
                        return Err(anyhow!(
                            "invalid --prover value: {value} (expected local or queue)"
                        ))
                    }
                });
            }
            _ if arg.starts_with('-') => {
                return Err(anyhow!("unknown flag in historical-root mode: {arg}"));
            }
            _ => positionals.push(PathBuf::from(arg)),
        }
    }

    if positionals.len() != 3 {
        return Err(anyhow!(
            "Usage: fixture-gen historical-root <batch_groth16.json> <committer_out.json> <consumer_out.json> [--prover local|queue] [--mock]"
        ));
    }

    Ok(Command::HistoricalRoot {
        batch_groth16_path: positionals.remove(0),
        committer_out: positionals.remove(0),
        consumer_out: positionals.remove(0),
        prover_choice,
        mock,
    })
}

fn parse_args() -> Result<Command> {
    let mut raw_args: Vec<String> = env::args().skip(1).collect();

    // Check for subcommands before flag parsing.
    if let Some(first) = raw_args.first() {
        match first.as_str() {
            "strip-calls" => {
                if raw_args.len() != 3 {
                    return Err(anyhow!(
                        "Usage: fixture-gen strip-calls <input.json> <output.json>"
                    ));
                }
                let output = PathBuf::from(raw_args.remove(2));
                let input = PathBuf::from(raw_args.remove(1));
                return Ok(Command::StripCalls { input, output });
            }
            "dump" => {
                if raw_args.len() != 2 {
                    return Err(anyhow!("Usage: fixture-gen dump <input.json>"));
                }
                let input = PathBuf::from(raw_args.remove(1));
                return Ok(Command::Dump { input });
            }
            "import-backend-result" => {
                let args = raw_args.into_iter().skip(1);
                return parse_import_backend_result_args(args);
            }
            "historical-root" => {
                let args = raw_args.into_iter().skip(1);
                return parse_historical_root_args(args);
            }
            "mockify" => {
                if raw_args.len() != 3 {
                    return Err(anyhow!(
                        "Usage: fixture-gen mockify <input.json> <output.json>"
                    ));
                }
                let output = PathBuf::from(raw_args.remove(2));
                let input = PathBuf::from(raw_args.remove(1));
                return Ok(Command::Mockify { input, output });
            }
            _ => {}
        }
    }

    let mut args = raw_args.into_iter();
    let mut debug_assumptions = false;
    let mut forwarder_mode: Option<ForwarderMode> = None;
    let mut nonce_seed: Option<u8> = None;
    let mut multi_external_call = false;
    let mut error_variants_dir: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut prover_choice: Option<ProverChoice> = None;
    let mut mock = false;

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
            "--debug-assumptions" => {
                debug_assumptions = true;
            }
            "--output-mismatch" | "--forwarder-fail" | "--forwarder-silent" => {
                if forwarder_mode.is_some() {
                    return Err(anyhow!(
                        "at most one of --output-mismatch, --forwarder-fail, --forwarder-silent may be set"
                    ));
                }
                forwarder_mode = Some(match flag {
                    "--output-mismatch" => ForwarderMode::BlockTimeForwarder {
                        output_mismatch: true,
                    },
                    "--forwarder-fail" => ForwarderMode::TestForwarderFail,
                    "--forwarder-silent" => ForwarderMode::TestForwarderSilent,
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
            "--mock" => {
                mock = true;
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
            "--prover" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--prover requires a value"))?;
                prover_choice = Some(match value.as_str() {
                    "local" => ProverChoice::Local,
                    "queue" => ProverChoice::Queue,
                    _ => {
                        return Err(anyhow!(
                            "invalid --prover value: {value} (expected local or queue)"
                        ))
                    }
                });
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

    Ok(Command::Generate(GenerateArgs {
        debug_assumptions,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
        error_variants_dir,
        out_path,
        prover_choice,
        mock,
    }))
}

/// Enter mock mode: proofs run through the local dev-mode executor (guests
/// execute, nothing is proven), and `finalize_and_write_fixture` turns the
/// resulting Fake aggregation receipt into a mock seal. RISC0_DEV_MODE is a
/// runtime env var read by risc0 at proving time; setting it here keeps the
/// flag self-contained instead of depending on ambient environment state.
fn apply_mock_mode(prover_choice: Option<ProverChoice>) -> Result<Option<ProverChoice>> {
    if matches!(prover_choice, Some(ProverChoice::Queue)) {
        bail!("--mock generates dev-mode receipts with the local executor; --prover queue is incompatible");
    }
    env::set_var("RISC0_DEV_MODE", "1");
    eprintln!("mode: mock (dev-mode receipts -> mock seal, selector 0xffffffff)");
    Ok(Some(ProverChoice::Local))
}

/// Resolve the `Prover` to use: an explicit `--prover` wins; otherwise default
/// to `Queue` when `QUEUE_BASE_URL` is set (preserving today's behavior when a
/// queue is configured), `Local` otherwise (so fixture-gen works out of the
/// box with no queue credentials).
fn resolve_prover(choice: Option<ProverChoice>) -> Result<Prover> {
    let choice = choice.unwrap_or_else(|| {
        if env::var("QUEUE_BASE_URL").is_ok() {
            ProverChoice::Queue
        } else {
            ProverChoice::Local
        }
    });
    match choice {
        ProverChoice::Local => Ok(Prover::Local),
        ProverChoice::Queue => Ok(Prover::Queue(build_queue_client()?)),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let GenerateArgs {
        debug_assumptions,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
        error_variants_dir,
        out_path,
        prover_choice,
        mock,
    } = match parse_args()? {
        Command::StripCalls { input, output } => return strip_calls_from_fixture(&input, &output),
        Command::Dump { input } => return dump_fixture(&input),
        Command::ImportBackendResult {
            input,
            output,
            root_account_dir,
            program_id,
        } => {
            return import_backend_result_fixture(
                &input,
                &output,
                root_account_dir.as_deref(),
                program_id,
            );
        }
        Command::HistoricalRoot {
            batch_groth16_path,
            committer_out,
            consumer_out,
            prover_choice,
            mock,
        } => {
            let prover_choice = if mock {
                apply_mock_mode(prover_choice)?
            } else {
                prover_choice
            };
            return generate_historical_root_fixtures(
                &batch_groth16_path,
                &committer_out,
                &consumer_out,
                prover_choice,
            )
            .await;
        }
        Command::Mockify { input, output } => {
            return mockify_fixture(&input, &output);
        }
        Command::Generate(args) => args,
    };

    let total_start = Instant::now();

    let prover_choice = if mock {
        apply_mock_mode(prover_choice)?
    } else {
        prover_choice
    };
    let prover = resolve_prover(prover_choice)?;
    match &prover {
        Prover::Local => eprintln!("prover: local (CPU risc0 prover)"),
        Prover::Queue(_) => eprintln!(
            "prover: queue ({})",
            env::var("QUEUE_BASE_URL").unwrap_or_default()
        ),
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

    eprintln!("phase: generate_test_transaction");
    let gen_start = Instant::now();
    let mut tx = generate_test_transaction_with_external_payload(
        &prover,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
    )
    .await?;
    eprintln!(
        "phase done: generate_test_transaction ({})",
        fmt_duration(gen_start.elapsed())
    );

    if debug_assumptions {
        timed_phase(
            "debug_assumptions (claim digests must match env::verify calls)",
            || debug_batch_assumptions(&tx),
        )?;
    }

    eprintln!("phase: aggregate_with_strategy(batch, groth16) (this is the expensive step)");
    let agg_start = Instant::now();
    tx = aggregate_tx(&prover, tx)
        .await
        .context("aggregate tx (batch, groth16)")?;
    eprintln!(
        "phase done: aggregate_with_strategy(batch, groth16) ({})",
        fmt_duration(agg_start.elapsed())
    );

    timed_phase("verify_aggregation", || {
        tx.verify_aggregation().context("verify aggregated proof")
    })?;

    let fixture = finalize_and_write_fixture(&mut tx, &out_path, None)?;

    if let Some(dir) = error_variants_dir.as_deref() {
        timed_phase("write_error_variants", || {
            generate_error_variant_fixtures(
                &tx,
                &fixture.selector,
                fixture.aggregation_proof_type,
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

fn compute_expected_claim_digest(journal: &[u8], vk: &Digest) -> risc0_zkvm::sha::Digest {
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
            // Wire instance is already journal bytes — no re-serialization needed.
            let journal = cu.instance.clone();
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
        let (tags, logics): (Vec<Digest>, Vec<Digest>) =
            solana_pa::encoding::extract_tags_and_logic_refs(action)
                .map_err(|e| anyhow!("extract tags / logic refs: {e:?}"))?;

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
    // Tests build transactions via the local arm prover directly (not through
    // `Prover::Local`/`spawn_blocking`) so they stay synchronous. Requires
    // `RISC0_DEV_MODE=1` to run.

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

    /// Helper: build a minimal valid transaction with a real delta proof.
    /// Uses the passthrough logic circuit and ephemeral resources.
    fn build_valid_tx_with_delta_proof(nonce_byte: u8) -> Transaction {
        let nf_key = NullifierKey::default();
        let nf_key_cm = nf_key.commit();
        let passthrough_vk = Digest(PASSTHROUGH_LOGIC_GUEST_ID);

        let mut consumed = Resource {
            logic_ref: passthrough_vk,
            nk_commitment: nf_key_cm,
            quantity: 1,
            is_ephemeral: true,
            ..Default::default()
        };
        consumed.nonce = [[nonce_byte; 16], [0u8; 16]].concat().try_into().unwrap();
        let consumed_nf = consumed.nullifier(&nf_key).unwrap();

        let mut created = consumed;
        created.set_nonce(consumed_nf);

        let witness = ComplianceWitness {
            consumed_resource: consumed,
            created_resource: created,
            merkle_path: MerklePath::empty(),
            rcv: Scalar::ONE.to_bytes().to_vec(),
            nf_key,
            ephemeral_root: initial_root(),
        };
        let cu = create_compliance_unit(&witness, LocalProofType::Succinct).unwrap();

        let inst = ComplianceInstance::from_journal(&cu.instance).unwrap();
        let consumed_nf = inst.consumed_nullifier;
        let created_cm = inst.created_commitment;
        let tags = vec![consumed_nf, created_cm];
        let tree = MerkleTree::from(tags);
        let root = tree.root().unwrap();

        let consumed_instance = LogicInstance {
            tag: consumed_nf,
            is_consumed: true,
            root,
            app_data: AppData::default(),
        };
        let created_instance = LogicInstance {
            tag: created_cm,
            is_consumed: false,
            root,
            app_data: AppData::default(),
        };

        let (cp, cj) = arm::proving_system::prove(
            PASSTHROUGH_LOGIC_GUEST_ELF,
            &consumed_instance,
            LocalProofType::Succinct,
        )
        .unwrap();
        let (crp, crj) = arm::proving_system::prove(
            PASSTHROUGH_LOGIC_GUEST_ELF,
            &created_instance,
            LocalProofType::Succinct,
        )
        .unwrap();

        let consumed_logic = LogicVerifier {
            proof: Some(cp),
            instance: cj,
            verifying_key: passthrough_vk,
        };
        let created_logic = LogicVerifier {
            proof: Some(crp),
            instance: crj,
            verifying_key: passthrough_vk,
        };

        let action = Action::new(vec![cu], vec![consumed_logic, created_logic]).unwrap();

        let delta_witness = DeltaWitness::from_bytes_vec(&[witness.rcv]).unwrap();
        let tx = Transaction::create(
            vec![action],
            Delta::Witness(CoreDeltaWitness(delta_witness.to_bytes())),
        );
        tx.generate_delta_proof(hash_delta_msg).unwrap()
    }

    /// A valid transaction must pass delta verification via the k256 path.
    #[test]
    fn valid_tx_passes_delta_verification() {
        let tx = build_valid_tx_with_delta_proof(100);
        tx.verify(hash_delta_msg).unwrap();
    }

    /// Swapping the nullifier and commitment tags in the delta message
    /// must invalidate the delta proof signature.
    #[test]
    fn swapped_tags_invalidate_delta_proof() {
        let tx = build_valid_tx_with_delta_proof(101);
        tx.clone().verify(hash_delta_msg).unwrap();

        // Swap nullifier and commitment in the compliance instance.
        // This changes the delta message (which is [nf, cm] → [cm, nf]),
        // invalidating the signature over the original message hash.
        let mut swapped = tx.clone();
        mutate_compliance_instance(&mut swapped.actions[0].compliance_units[0], |inst| {
            std::mem::swap(&mut inst.consumed_nullifier, &mut inst.created_commitment);
        })
        .unwrap();

        let result = swapped.verify(hash_delta_msg);
        assert!(result.is_err(), "swapped nf/cm must invalidate delta proof");
    }

    /// Mutating a single delta coordinate must invalidate the delta proof.
    #[test]
    fn mutated_delta_x_invalidates_proof() {
        let mut tx = build_valid_tx_with_delta_proof(102);
        tx.clone().verify(hash_delta_msg).unwrap();

        // Flip a word in delta_x
        mutate_compliance_instance(&mut tx.actions[0].compliance_units[0], |inst| {
            inst.delta_x[0] ^= 0xFFFFFFFF;
        })
        .unwrap();

        let result = tx.verify(hash_delta_msg);
        assert!(
            result.is_err(),
            "mutated delta_x must invalidate delta proof"
        );
    }

    /// Mutating a nullifier must invalidate the delta proof (changes the message hash).
    #[test]
    fn mutated_nullifier_invalidates_delta_proof() {
        let mut tx = build_valid_tx_with_delta_proof(103);
        tx.clone().verify(hash_delta_msg).unwrap();

        // Corrupt the nullifier
        mutate_compliance_instance(&mut tx.actions[0].compliance_units[0], |inst| {
            inst.consumed_nullifier = Digest::from_bytes([0xFF; 32]);
        })
        .unwrap();

        let result = tx.verify(hash_delta_msg);
        assert!(
            result.is_err(),
            "mutated nullifier must invalidate delta proof"
        );
    }
}
