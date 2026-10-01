use anchor_lang::prelude::{borsh, AnchorDeserialize as BorshDeserialize, Pubkey};
use anoma_pa_solana_client::merkle::merkle_path;
use anyhow::{anyhow, bail, Context, Result};
use arm::action::Action;
use arm::action_tree::ActionTree;
use arm::aggregation_instance::ConsumedResourceAggregated;
use arm::compliance::{ComplianceWitness, INITIAL_ROOT};
use arm::compliance_unit::ComplianceUnit;
use arm::constants::{
    init_kind_table_from_file, kind_table, kind_table_hash, BATCH_AGGREGATION_PK,
    BATCH_AGGREGATION_VK, COMPLIANCE_PK, COMPLIANCE_VK,
};
use arm::logic_instance::ExpirableBlob;
use arm::logic_instance::{AppData, LogicInstance};
use arm::logic_proof::LogicVerifier;
use arm::merkle_path::MerklePath;
use arm::nullifier_key::NullifierKey;
use arm::proving_system::{encode_seal, JournalEncoding, ProofType as LocalProofType};
use arm::resource::{ConsumedResourceWitness, Resource};
use arm::transaction::{Aggregation, Delta, Transaction};
use arm::Digest;
use arm_gadgets::authority::{AuthoritySigningKey, AuthorityVerifyingKey};
use arm_gadgets::encryption::{generate_public_key, SecretKey};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures::future::try_join_all;
use heliax_ap_orchestrator_sdk::{
    AggregateProofResult, BaseProofResult, GpuAggregationProofPayload, GpuComplianceProofPayload,
    GpuLogicProofPayload, ProofPayload, ProofType as QueueProofType, QueueClient,
};
use k256::{AffinePoint, Scalar};
use risc0_zkvm::sha::{Digestible as _, Sha256 as _};
use risc0_zkvm::{InnerReceipt, MaybePruned, Receipt, ReceiptClaim};
use serde::Serialize;
use solana_pa::verifier_router::types::{Proof, Seal};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use passthrough_logic_methods::{PASSTHROUGH_LOGIC_GUEST_ELF, PASSTHROUGH_LOGIC_GUEST_ID};

/// The kind table a fixture commits to unless `--kind-table` names another:
/// the committed empty table. The compliance circuit hashes the witness's
/// table into the instance, and the PA pins that commitment at
/// initialization, so fixtures and deployment tooling must agree on this file.
const KIND_TABLE_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/kind_table.json");

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
        proof: result.receipt,
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
/// images. Returns a transaction with `aggregation` populated and the
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

/// How a prover runs independent proving jobs.
#[derive(Clone, Copy)]
enum JobScheduling {
    /// All at once: the queue proves them on its own workers.
    Concurrent,
    /// One at a time: concurrent local proofs exhaust the machine's CPU and
    /// memory.
    Sequential,
}

impl Prover {
    fn scheduling(&self) -> JobScheduling {
        match self {
            Prover::Queue(_) => JobScheduling::Concurrent,
            Prover::Local => JobScheduling::Sequential,
        }
    }
}

/// Run independent jobs under `scheduling`, returning their results in input
/// order. A job does no work until it is awaited, so `Sequential` starts
/// each only after the previous one finished.
async fn run_jobs<T>(
    scheduling: JobScheduling,
    jobs: impl IntoIterator<Item = impl Future<Output = Result<T>>>,
) -> Result<Vec<T>> {
    match scheduling {
        JobScheduling::Concurrent => try_join_all(jobs).await,
        JobScheduling::Sequential => {
            let mut results = Vec::new();
            for job in jobs {
                results.push(job.await?);
            }
            Ok(results)
        }
    }
}

/// Run two independent jobs of different result types under `scheduling`.
async fn run_job_pair<A, B>(
    scheduling: JobScheduling,
    first: impl Future<Output = Result<A>>,
    second: impl Future<Output = Result<B>>,
) -> Result<(A, B)> {
    match scheduling {
        JobScheduling::Concurrent => futures::try_join!(first, second),
        JobScheduling::Sequential => Ok((first.await?, second.await?)),
    }
}

/// Prove one compliance witness locally on CPU. Runs on tokio's blocking
/// thread pool since risc0 proving is synchronous, CPU-bound work.
async fn local_compliance_proof(witness: &ComplianceWitness) -> Result<ComplianceUnit> {
    let witness = witness.clone();
    tokio::task::spawn_blocking(move || {
        arm::compliance_unit::create(&witness, LocalProofType::Succinct)
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
        arm::transaction::aggregate(
            &mut tx,
            LocalProofType::Groth16,
            JournalEncoding::Risc0Serde,
        )
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

use block_time_forwarder::{RESULT_GT, RESULT_LT};
use ed25519_dalek::{Signer, SigningKey};
use rand_chacha::rand_core::SeedableRng;
use rand_chacha::ChaCha20Rng;
use solana_pa::external_calls::encode_external_call;
use solana_pa::state::PAStateAccount;
use solana_pa::types::{OutputMode, SolanaExternalCall};
use test_forwarder::{MODE_FAIL, MODE_RELAY, MODE_SILENT, RELAY_OK};
use transfer_library::action::{self, ComplianceParams, Owner, TransferAction, Wrap, WrapAuth};
use transfer_library::{TOKEN_TRANSFER_ELF, TOKEN_TRANSFER_ID};
use transfer_witness::{LabelInfo, ValueInfo, WrapAuthInfo, AUTH_SIGNATURE_DOMAIN};

/// Everything the SPL forwarder wrap test needs to replay the fixture's
/// external call: the seeded user and mint keypairs, the wrap terms, and the
/// signature the fixture's proof is bound to.
/// The actors are keypairs seeded with sha256 of a label, so the fixture
/// carries the labels and the test rebuilds the keypairs: no key material
/// is stored, only the public recipe.
#[derive(Clone, Debug, Serialize)]
struct SplTokenWrapMetadata {
    user_seed_label: &'static str,
    mint_seed_label: &'static str,
    amount: u64,
    nonce: u64,
    /// The 44 bytes the user signed: base64 of the wrap message hash.
    signed_message_b64: String,
    signature_b64: String,
    logic_ref_b64: String,
}

#[derive(Clone, Debug, Serialize)]
struct SplTokenUnwrapMetadata {
    mint_seed_label: &'static str,
    amount: u64,
    /// The seeded recipient, or none when the unwrap releases to the
    /// forwarder's escrow authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    recipient_seed_label: Option<&'static str>,
    logic_ref_b64: String,
}

/// The SPL forwarder replay data an AnomaPay fixture carries beside its
/// transaction, serialized under the `spl_token_wrap` or `spl_token_unwrap`
/// key.
#[derive(Serialize)]
enum SplForwarderMetadata {
    #[serde(rename = "spl_token_wrap")]
    Wrap(SplTokenWrapMetadata),
    #[serde(rename = "spl_token_unwrap")]
    Unwrap(SplTokenUnwrapMetadata),
}

#[derive(Serialize)]
struct Fixture {
    format: &'static str,
    aggregation_strategy: &'static str,
    aggregation_proof_type: &'static str,
    /// Groth16 verifier selector extracted from the proof's verifier_parameters.
    /// Format: "0x" + 4-byte hex (e.g., "0x73c457ba").
    selector: String,
    #[serde(flatten)]
    spl_forwarder: Option<SplForwarderMetadata>,
    tx_b64: String,
    tx_tampered_b64: String,
    consumed_nullifiers_b64: Vec<String>,
    /// Created commitments in instance order — the leaves settlement appends.
    /// Clients derive the produced-root marker address from these plus the
    /// on-chain tree state.
    created_commitments_b64: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    historical_roots_b64: Vec<String>,
}

const FIXTURE_FORMAT: &str = "arm-risc0:Transaction(bincode)";

/// Generate the Solana PA's test fixtures: proven, aggregated transactions
/// the integration suite settles.
#[derive(Parser)]
#[command(name = "fixture-gen")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Generate(ShapeCommand),
    /// The historical-root pair: a committer settled right after
    /// batch_groth16, and a consumer spending its resource through a real
    /// Merkle path over the tree [batch_groth16, committer].
    HistoricalRoot {
        /// The settled batch_groth16 fixture (leaf 0 of the tree).
        batch_groth16: PathBuf,
        /// Where to write the committer fixture.
        committer_out: PathBuf,
        /// Where to write the consumer fixture.
        consumer_out: PathBuf,
        #[command(flatten)]
        prover: ProverArgs,
    },
    /// Print a fixture's transaction structure.
    Dump { input: PathBuf },
}

/// One fixture shape per subcommand, each taking only its own options.
#[derive(Subcommand)]
enum ShapeCommand {
    /// One action whose consumed resource calls the block-time forwarder.
    Batch {
        /// Append a second block-time-forwarder external call.
        #[arg(long)]
        multi_external_call: bool,
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The block-time-forwarder call with a wrong expected output, for the
    /// ExternalCallOutputMismatch test.
    OutputMismatch {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder with a failing instruction.
    ForwarderFail {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder returning no data.
    ForwarderSilent {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder relaying an unwrap to the SPL forwarder, which must
    /// reject a caller other than the adapter.
    ForwarderRelay {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// One action that consumes a zero-quantity ephemeral resource and
    /// creates nothing: a settlement that appends no commitment.
    ConsumeOnly {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// Three single-unit actions with event-emitted payload blobs and no
    /// external calls: the captured mainnet transfer's shape.
    TransferShape {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// An AnomaPay wrap proven with the transfer logic: the user's
    /// ed25519-authorized escrow deposit creates the owner's resource.
    SplTokenWrap {
        /// The forwarder nonce the user signs.
        #[arg(long, value_name = "N", default_value_t = ANOMAPAY_WRAP_NONCE)]
        wrap_nonce: u64,
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// An AnomaPay unwrap: the owner spends the wrapped resource, releasing
    /// the escrow to the recipient.
    SplTokenUnwrap {
        /// The wrap fixture, settled alone on a fresh adapter: its created
        /// commitments are the tree the unwrap proves membership in.
        #[arg(long, value_name = "FIXTURE")]
        wrap: PathBuf,
        /// Release the tokens to the forwarder's own escrow authority: the
        /// unwrap the forwarder refuses, as the EVM forwarder reverts an
        /// unwrap to itself.
        #[arg(long)]
        to_escrow: bool,
        #[command(flatten)]
        generate: GenerateArgs,
    },
}

/// The options every generated fixture takes.
#[derive(Args)]
struct GenerateArgs {
    /// Where to write the fixture. Every resource nonce derives from its file
    /// stem, so fixtures with different names never share a nullifier.
    out_path: PathBuf,
    #[command(flatten)]
    prover: ProverArgs,
    /// Also write the final transaction's error variants to DIR:
    /// wrong_root, no_aggregation, garbage_proof, corrupt_seal, zero_action
    /// and witness_delta.
    #[arg(long, value_name = "DIR")]
    error_variants: Option<PathBuf>,
    /// Prove against this kind table instead of the committed empty one
    /// (kind_table.json); the PA must store its commitment.
    #[arg(long, value_name = "PATH", default_value = KIND_TABLE_PATH)]
    kind_table: PathBuf,
    /// Print claim digests for composition debugging.
    #[arg(long)]
    debug_assumptions: bool,
}

#[derive(Args)]
struct ProverArgs {
    /// Run the dev-mode executor instead of proving (seconds, no GPU or
    /// container proving step), and emit a mock seal (selector 0xffffffff)
    /// only the localnet mock verifier accepts.
    #[arg(long, conflicts_with = "prover")]
    mock: bool,
    /// The prover backend; defaults to queue when QUEUE_BASE_URL is set,
    /// local otherwise. local runs risc0's CPU prover in-process (its Groth16
    /// step needs podman/docker); queue dispatches to the AnomaPay workers
    /// queue at QUEUE_BASE_URL, authenticated with QUEUE_AUTH_TOKEN.
    #[arg(long, value_enum)]
    prover: Option<ProverChoice>,
}

/// Explicit `--prover` selection. `None` (the flag was not passed) resolves
/// to `Queue` if `QUEUE_BASE_URL` is set in the environment, `Local`
/// otherwise — see `resolve_prover`.
#[derive(Clone, Copy, ValueEnum)]
enum ProverChoice {
    Local,
    Queue,
}

enum ForwarderMode {
    BlockTimeForwarder {
        output_mismatch: bool,
        multi_external_call: bool,
    },
    TestForwarderFail,
    TestForwarderSilent,
    TestForwarderRelay,
}

/// The aggregation carried by a transaction, or a clear error if it has none.
fn require_aggregation(tx: &Transaction) -> Result<&Aggregation> {
    tx.aggregation
        .as_ref()
        .ok_or_else(|| anyhow!("transaction has no aggregation"))
}

fn require_aggregation_mut(tx: &mut Transaction) -> Result<&mut Aggregation> {
    tx.aggregation
        .as_mut()
        .ok_or_else(|| anyhow!("transaction has no aggregation"))
}

/// Extract the Groth16 selector from a transaction's seal-encoded
/// aggregation proof.
fn extract_selector(tx: &Transaction) -> Result<String> {
    let seal: Seal = Seal::try_from_slice(&require_aggregation(tx)?.proof)
        .context("decode Seal from aggregation proof bytes")?;
    Ok(format!("0x{}", hex::encode(seal.selector)))
}

/// Selector the localnet mock verifier is registered under in the synthetic
/// VerifierEntry preloaded at test-validator genesis (risc0 fake-receipt
/// convention; the real Groth16 selector is 0x73c457ba).
const MOCK_SELECTOR: [u8; 4] = [0xff; 4];

/// Claim digest a mock seal must carry, derived from the transaction alone:
/// the digest of the batch-aggregation receipt claim over the aggregation
/// instance's journal, which the on-chain PA independently recomputes at
/// settle time.
fn mock_claim_digest(tx: &Transaction) -> Result<risc0_zkvm::sha::Digest> {
    let journal = require_aggregation(tx)?.instance.to_journal();
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
        proof: Proof {
            pi_a: [0u8; 64],
            pi_b: [0u8; 128],
            pi_c,
        },
    };
    borsh::to_vec(&seal).context("serialize mock Seal")
}

/// Encode a dev-mode (Fake) aggregation receipt as a mock router seal,
/// cross-checking the receipt's claim digest against the one derived from
/// the transaction alone so any journal-derivation drift fails loudly.
fn encode_mock_seal(
    fake: &risc0_zkvm::FakeReceipt<ReceiptClaim>,
    tx: &Transaction,
) -> Result<Vec<u8>> {
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

/// Flip one bit of a tag of the aggregation instance's first action: its
/// first created commitment, or its first consumed nullifier when it creates
/// nothing. The transaction still decodes and settles structurally, but the
/// journal digest the PA recomputes from the mutated instance no longer
/// matches the proof, so verification must fail.
fn mutate_tag_keep_structure(tx: &mut Transaction) -> Result<()> {
    let instance = &mut require_aggregation_mut(tx)?.instance;
    let action = instance
        .actions
        .get_mut(0)
        .ok_or_else(|| anyhow!("aggregation instance has no actions"))?;
    let tag = match action.created_publics.first_mut() {
        Some(created) => &mut created.resource_commitment,
        None => {
            &mut action
                .consumed_publics
                .first_mut()
                .ok_or_else(|| anyhow!("the first action has no resources"))?
                .resource_nullifier
        }
    };

    let mut bytes = <[u8; 32]>::from(*tag);
    bytes[0] ^= 1;
    *tag = Digest::from_bytes(bytes);
    Ok(())
}

fn block_time_forwarder_external_payload_blob(output_mismatch: bool) -> Result<ExpirableBlob> {
    let program_id = block_time_forwarder::ID.to_bytes();

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
        program_id: test_forwarder::ID.to_bytes(),
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
        program_id: test_forwarder::ID.to_bytes(),
        instruction_data: vec![MODE_SILENT],
        expected_output: vec![0x2a],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    }))
}

/// Amount the relay fixture's unwrap names; it is settled once the escrow holds tokens.
const RELAY_UNWRAP_AMOUNT: u64 = 1;

/// A test-forwarder call relaying an unwrap to the SPL token forwarder under
/// the transfer logic ref its config authorizes: the seeded mint, to the
/// seeded recipient. Segment: the test-forwarder, then the SPL forwarder's
/// unwrap segment (program, config, instructions sysvar, escrow ATA,
/// recipient ATA, escrow authority, token program).
fn test_forwarder_relay_payload_blob() -> Result<ExpirableBlob> {
    let seeded_pubkey =
        |label| Pubkey::new_from_array(seeded_keypair(label).verifying_key().to_bytes());
    let unwrap = spl_token_forwarder::UnwrapInput {
        token_mint: seeded_pubkey(MINT_SEED_LABEL),
        amount: RELAY_UNWRAP_AMOUNT,
        recipient: seeded_pubkey(RECIPIENT_SEED_LABEL),
    };
    let mut instruction_data = vec![MODE_RELAY];
    instruction_data.extend_from_slice(TOKEN_TRANSFER_ID.as_bytes());
    instruction_data.push(spl_token_forwarder::OP_UNWRAP);
    instruction_data.extend_from_slice(&unwrap.to_bytes());
    Ok(encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder::ID.to_bytes(),
        instruction_data,
        expected_output: vec![RELAY_OK],
        output_mode: OutputMode::ReturnData,
        num_accounts: anoma_pa_solana_client::FORWARDER_UNWRAP_NUM_ACCOUNTS + 1,
    }))
}

const USER_SEED_LABEL: &str = "spl_token_forwarder_test_user";
const MINT_SEED_LABEL: &str = "spl_token_forwarder_test_mint";
const RECIPIENT_SEED_LABEL: &str = "spl_token_forwarder_test_recipient";
/// The shielded owner of the wrapped resource. Like the Solana actors, the
/// keys are seeded from labels so the fixture carries no key material.
const OWNER_AUTH_SEED_LABEL: &str = "spl_token_forwarder_test_owner_auth";
const OWNER_ENCRYPTION_SEED_LABEL: &str = "spl_token_forwarder_test_owner_encryption";
const OWNER_NF_KEY_SEED_LABEL: &str = "spl_token_forwarder_test_owner_nf_key";
const DISCOVERY_SEED_LABEL: &str = "spl_token_forwarder_test_discovery";
const WRAPPED_RAND_SEED_LABEL: &str = "spl_token_forwarder_test_wrapped_rand_seed";

/// 100 tokens at 6 decimals: the wrap deposits it, the unwrap releases it.
const ANOMAPAY_AMOUNT: u64 = 100_000_000;
const ANOMAPAY_WRAP_NONCE: u64 = 1;
/// Year 2100: the deadline never expires under test.
const ANOMAPAY_WRAP_DEADLINE: i64 = 4_102_444_800;
/// The suite puts the ed25519 instruction first in the settlement.
const ANOMAPAY_ED25519_IX_INDEX: u8 = 0;

/// A keypair seeded with sha256 of a label, as the TypeScript test's
/// `seededKeypair(label)` rebuilds it.
fn seeded_keypair(label: &str) -> SigningKey {
    SigningKey::from_bytes(&label_hash(label))
}

fn label_hash(label: &str) -> [u8; 32] {
    arm::utils::hash_bytes(label.as_bytes()).into()
}

/// The randomness a fixture's created resources encrypt with: a ChaCha20
/// stream seeded from the fixture's name, so the fixture regenerates
/// byte-identical.
fn fixture_rng(fixture_name: &str) -> ChaCha20Rng {
    ChaCha20Rng::from_seed(label_hash(&format!(
        "solana-pa/fixture-gen/encryption-rng/{fixture_name}"
    )))
}

/// A secp256k1 scalar from a label: its hash, which is a valid scalar.
fn scalar_from_label(label: &str) -> Scalar {
    *k256::SecretKey::from_slice(&label_hash(label))
        .expect("a sha256 output is a valid secp256k1 scalar")
        .to_nonzero_scalar()
}

/// The wrapped resource's owner: the keys the resource commits to, and the
/// authorization signing key behind them.
struct SeededOwner {
    auth_sk: AuthoritySigningKey,
    keys: Owner,
}

impl SeededOwner {
    fn seeded() -> Self {
        let auth_sk = AuthoritySigningKey::from_bytes(&label_hash(OWNER_AUTH_SEED_LABEL))
            .expect("a sha256 output is a valid secp256k1 scalar");
        let encryption_sk = SecretKey::new(scalar_from_label(OWNER_ENCRYPTION_SEED_LABEL));
        SeededOwner {
            keys: Owner {
                value: ValueInfo {
                    auth_pk: AuthorityVerifyingKey::from_signing_key(&auth_sk),
                    encryption_pk: generate_public_key(encryption_sk.inner()),
                },
                nf_key: NullifierKey::from_bytes(label_hash(OWNER_NF_KEY_SEED_LABEL)),
            },
            auth_sk,
        }
    }
}

fn discovery_pk() -> AffinePoint {
    generate_public_key(SecretKey::new(scalar_from_label(DISCOVERY_SEED_LABEL)).inner())
}

/// The Solana parties of the AnomaPay fixtures and the resource label they
/// share: the forwarder and the mint.
struct AnomaPayActors {
    user: SigningKey,
    recipient: [u8; 32],
    label: LabelInfo,
}

impl AnomaPayActors {
    fn seeded() -> Self {
        AnomaPayActors {
            user: seeded_keypair(USER_SEED_LABEL),
            recipient: seeded_keypair(RECIPIENT_SEED_LABEL)
                .verifying_key()
                .to_bytes(),
            label: LabelInfo {
                forwarder_program_id: spl_token_forwarder::ID.to_bytes(),
                spl_token_mint: seeded_keypair(MINT_SEED_LABEL).verifying_key().to_bytes(),
            },
        }
    }
}

fn token_transfer_logic_ref_b64() -> String {
    BASE64.encode(TOKEN_TRANSFER_ID.as_bytes())
}

/// The compliance facts of every AnomaPay fixture: a fixed `rcv`, so the
/// fixtures are deterministic, and the committed kind table the PA pins.
fn anomapay_compliance_params() -> ComplianceParams {
    ComplianceParams {
        rcv: Scalar::ONE.to_bytes().to_vec(),
        kind_table: kind_table().to_vec(),
    }
}

/// The seeded wrap of 100 tokens that the fixture `fixture_name` settles:
/// its consumed resource carries the fixture's first nonce. Returns the wrap
/// and the parties behind it.
fn seeded_wrap(fixture_name: &str) -> Result<(AnomaPayActors, SeededOwner, Wrap)> {
    let actors = AnomaPayActors::seeded();
    let owner = SeededOwner::seeded();
    let wrap = action::wrap(
        actors.label.clone(),
        ANOMAPAY_AMOUNT,
        fixture_nonce(fixture_name, 0),
        owner.keys.clone(),
        label_hash(WRAPPED_RAND_SEED_LABEL),
    )
    .map_err(|e| anyhow!("build the wrap's resources: {e:?}"))?;
    Ok((actors, owner, wrap))
}

/// Prove one AnomaPay action through the selected prover and wrap it in a
/// balanced transaction.
async fn prove_anomapay_action(prover: &Prover, action: TransferAction) -> Result<Transaction> {
    let proven = prove_compliance_and_logic(
        prover,
        &action.compliance_witness,
        TOKEN_TRANSFER_ELF,
        &TOKEN_TRANSFER_ID,
        action.consumed_logic.witness,
        action.created_logic.witness,
    )
    .await
    .context("prove the AnomaPay action")?;
    assemble_transaction(vec![proven], &[action.compliance_witness.rcv])
}

/// The AnomaPay wrap the fixture settles: the seeded user wraps 100 tokens
/// of the seeded mint, consuming an ephemeral resource whose transfer logic
/// commits the forwarder call, and creating the owner's shielded resource
/// whose logic emits the encrypted payloads. The user signs the message the
/// resource derives from the proof-bound input, which the forwarder
/// recomputes at settlement. `wrap_nonce` is the forwarder nonce the user
/// signs, independent of the resource nonce the fixture name derives.
async fn generate_anomapay_wrap_transaction(
    prover: &Prover,
    fixture_name: &str,
    wrap_nonce: u64,
) -> Result<(Transaction, SplForwarderMetadata)> {
    let (actors, _, wrap) = seeded_wrap(fixture_name)?;
    let auth = WrapAuth {
        user: actors.user.verifying_key().to_bytes(),
        info: WrapAuthInfo {
            nonce: wrap_nonce,
            deadline: ANOMAPAY_WRAP_DEADLINE,
            ed25519_ix_index: ANOMAPAY_ED25519_IX_INDEX,
        },
    };
    let signed_message = wrap
        .signed_message(&auth)
        .map_err(|e| anyhow!("derive the wrap's signed message: {e:?}"))?;
    let signature = actors.user.sign(signed_message.as_bytes()).to_bytes();
    let action = wrap
        .action(
            auth,
            &discovery_pk(),
            anomapay_compliance_params(),
            &mut fixture_rng(fixture_name),
        )
        .map_err(|e| anyhow!("build the wrap's witnesses: {e:?}"))?;
    let tx = prove_anomapay_action(prover, action).await?;

    let metadata = SplForwarderMetadata::Wrap(SplTokenWrapMetadata {
        user_seed_label: USER_SEED_LABEL,
        mint_seed_label: MINT_SEED_LABEL,
        amount: ANOMAPAY_AMOUNT,
        nonce: wrap_nonce,
        signed_message_b64: BASE64.encode(signed_message),
        signature_b64: BASE64.encode(signature),
        logic_ref_b64: token_transfer_logic_ref_b64(),
    });
    Ok((tx, metadata))
}

/// The adapter's commitment tree after appending `leaves` in order, replayed
/// through the program's own `append_to_tree` on a fresh state, so the root
/// carries the on-chain growth rule rather than a second implementation.
fn pa_tree_root(leaves: &[Digest]) -> Result<Digest> {
    let mut state = PAStateAccount::running(0, Pubkey::default(), Pubkey::default(), [0; 4]);
    for leaf in leaves {
        solana_pa::merkle::append_to_tree(&mut state, *leaf)
            .map_err(|e| anyhow!("replay append_to_tree: {e:?}"))?;
    }
    Ok(Digest::from_bytes(state.root))
}

/// The Merkle path of leaf `index` in the adapter's tree over `leaves`, as
/// the client library derives it for integrators.
fn pa_merkle_path(leaves: &[Digest], index: usize) -> MerklePath {
    let leaves: Vec<[u8; 32]> = leaves.iter().map(|leaf| (*leaf).into()).collect();
    let path: Vec<(Digest, bool)> = merkle_path(&leaves, index)
        .into_iter()
        .map(|(sibling, leaf_is_on_right)| (Digest::from_bytes(sibling), leaf_is_on_right))
        .collect();
    MerklePath::from_path(&path)
}

/// The Merkle path of leaf `index` and the root it reconstructs, checked
/// against the adapter's own replay of the tree: a client path that
/// disagrees with the program's growth rule fails here, not at settlement.
fn checked_pa_merkle_path(leaves: &[Digest], index: usize) -> Result<(MerklePath, Digest)> {
    let path = pa_merkle_path(leaves, index);
    let expected_root = pa_tree_root(leaves)?;
    let path_root = path.root(&leaves[index]);
    if path_root != expected_root {
        bail!(
            "the Merkle path of leaf {index} does not reconstruct the adapter's root: \
             path {} vs append_to_tree {}",
            hex::encode(path_root.as_bytes()),
            hex::encode(expected_root.as_bytes())
        );
    }
    Ok((path, expected_root))
}

/// The AnomaPay unwrap the fixture settles: the owner spends the resource
/// the wrap fixture `wrap_fixture_name` created, the last of `leaves` (the
/// tree settled before the unwrap), into an ephemeral resource whose
/// transfer logic commits the unwrap call releasing the 100 tokens to the
/// seeded recipient.
async fn generate_anomapay_unwrap_transaction(
    prover: &Prover,
    wrap_fixture_name: &str,
    leaves: &[Digest],
    to_escrow: bool,
) -> Result<(Transaction, SplForwarderMetadata)> {
    let (actors, owner, wrap) = seeded_wrap(wrap_fixture_name)?;
    let consumed_cm = wrap.created.commitment();
    let index = leaves
        .len()
        .checked_sub(1)
        .ok_or_else(|| anyhow!("the unwrap needs the leaves settled before it, the wrap last"))?;
    if leaves[index] != consumed_cm {
        bail!(
            "the last leaf must be the wrap's created commitment {}, found {}",
            hex::encode(consumed_cm.as_bytes()),
            hex::encode(leaves[index].as_bytes())
        );
    }
    let (path, root) = checked_pa_merkle_path(leaves, index)?;
    eprintln!(
        "verified: the wrapped resource is leaf {index} of {}; root {}",
        leaves.len(),
        hex::encode(root.as_bytes())
    );

    let recipient = if to_escrow {
        spl_token_forwarder::ESCROW_AUTHORITY.to_bytes()
    } else {
        actors.recipient
    };
    let unwrap = action::unwrap(
        actors.label.clone(),
        wrap.created,
        owner.keys.clone(),
        recipient,
    )
    .map_err(|e| anyhow!("build the unwrap's resources: {e:?}"))?;
    let action_tree_root = unwrap
        .action_tree_root()
        .map_err(|e| anyhow!("compute the unwrap's action tree root: {e:?}"))?;
    let auth_sig = owner
        .auth_sk
        .sign(AUTH_SIGNATURE_DOMAIN, action_tree_root.as_bytes());
    let action = unwrap
        .action(auth_sig, path, anomapay_compliance_params())
        .map_err(|e| anyhow!("build the unwrap's witnesses: {e:?}"))?;
    let tx = prove_anomapay_action(prover, action).await?;

    let metadata = SplForwarderMetadata::Unwrap(SplTokenUnwrapMetadata {
        mint_seed_label: MINT_SEED_LABEL,
        amount: ANOMAPAY_AMOUNT,
        recipient_seed_label: (!to_escrow).then_some(RECIPIENT_SEED_LABEL),
        logic_ref_b64: token_transfer_logic_ref_b64(),
    });
    Ok((tx, metadata))
}

/// The action tree root of a one-consumed, one-created action: nullifier
/// then commitment, the order the aggregation guest enforces.
fn single_action_tree_root(consumed_nf: Digest, created_cm: Digest) -> Result<Digest> {
    ActionTree::new(vec![consumed_nf, created_cm])
        .root()
        .map_err(|e| anyhow!("compute action tree root: {e:?}"))
}

/// Domain tag of the resource nonces derived from fixture names.
const FIXTURE_NONCE_DOMAIN: &[u8] = b"solana-pa/fixture-gen/resource-nonce";

/// The nonce of the `index`-th resource the fixture `fixture_name` generates:
/// sha256 of the domain tag, the length-prefixed name, and the index. Two
/// differently named fixtures never share a nonce, so never a nullifier, and
/// a fixture's nonces are the same on every run.
fn fixture_nonce(fixture_name: &str, index: u32) -> [u8; 32] {
    let mut preimage = FIXTURE_NONCE_DOMAIN.to_vec();
    preimage.extend_from_slice(&(fixture_name.len() as u64).to_le_bytes());
    preimage.extend_from_slice(fixture_name.as_bytes());
    preimage.extend_from_slice(&index.to_le_bytes());
    arm::utils::hash_bytes(&preimage).into()
}

/// A fixture's name: its file stem (`batch_groth16` for
/// `tests/fixtures/batch_groth16.json`), which its resource nonces derive from.
fn fixture_name(path: &Path) -> Result<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            anyhow!(
                "{} has no UTF-8 file stem to name the fixture",
                path.display()
            )
        })
}

/// The nonce of an action's first created resource: the compliance circuit
/// requires created nonces to be derived from the action's consumed nullifiers.
fn first_created_nonce(consumed_nf: Digest) -> Result<[u8; 32]> {
    Resource::derive_nonce_from_nullifiers(0, &[consumed_nf])
        .map_err(|e| anyhow!("derive created nonce: {e:?}"))
}

/// The `index`-th passthrough-logic ephemeral resource of the fixture
/// `fixture_name` (nonce from `fixture_nonce`), its nullifier under the
/// default nullifier key, and the resource its action creates: the same
/// resource under `first_created_nonce`.
fn deterministic_ephemeral_resource(
    fixture_name: &str,
    index: u32,
) -> Result<(Resource, NullifierKey, Digest, Resource)> {
    let nf_key = NullifierKey::default();
    let consumed_resource = Resource {
        logic_ref: Digest::new(PASSTHROUGH_LOGIC_GUEST_ID),
        quantity: 1,
        is_ephemeral: true,
        nonce: fixture_nonce(fixture_name, index),
        nk_commitment: nf_key.commit(),
        ..Default::default()
    };
    let consumed_nf = consumed_resource
        .nullifier(&nf_key)
        .map_err(|e| anyhow!("compute consumed nullifier: {e:?}"))?;
    let mut created_resource = consumed_resource;
    created_resource.nonce = first_created_nonce(consumed_nf)?;
    Ok((consumed_resource, nf_key, consumed_nf, created_resource))
}

/// Build the compliance witness for a single-consumed / single-created
/// action. Built through `from_parts` with a fixed, caller-chosen `rcv`
/// rather than arm's randomized `from_resources*` constructors, which draw
/// a fresh `rcv` and would make fixture generation nondeterministic.
/// Carries the globally loaded kind table, so every instance commits to
/// the table hash the PA pins at initialization.
fn single_action_compliance_witness(
    consumed: Resource,
    cm_merkle_path: MerklePath,
    nf_key: NullifierKey,
    created: Resource,
    rcv: Scalar,
) -> ComplianceWitness {
    ComplianceWitness::from_parts(
        vec![ConsumedResourceWitness {
            resource: consumed,
            cm_merkle_path,
            nf_key,
        }],
        vec![created],
        INITIAL_ROOT,
        &rcv.to_bytes(),
        kind_table().to_vec(),
    )
}

/// Prove one action (one compliance unit, one consumed + one created
/// resource): the compliance proof plus one passthrough logic proof per
/// resource, each carrying the given app_data.
async fn prove_action(
    prover: &Prover,
    compliance_witness: &ComplianceWitness,
    consumed_app_data: AppData,
    created_app_data: AppData,
) -> Result<Action> {
    let passthrough_vk = Digest::new(PASSTHROUGH_LOGIC_GUEST_ID);

    let consumed = &compliance_witness.consumed_data[0];
    let consumed_nf = consumed
        .resource
        .nullifier(&consumed.nf_key)
        .map_err(|e| anyhow!("compute consumed nullifier: {e:?}"))?;
    let created_cm = compliance_witness.created_resources[0].commitment();

    let root = single_action_tree_root(consumed_nf, created_cm)?;

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
        app_data: created_app_data,
    };

    prove_compliance_and_logic(
        prover,
        compliance_witness,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        consumed_instance,
        created_instance,
    )
    .await
    .context("prove the passthrough action")
}

/// Prove an action's compliance unit and its consumed and created
/// resources' logic under one guest, and build the action from the proofs.
async fn prove_compliance_and_logic<T: Serialize + Send + 'static>(
    prover: &Prover,
    compliance_witness: &ComplianceWitness,
    proving_key: &'static [u8],
    verifying_key: &Digest,
    consumed: T,
    created: T,
) -> Result<Action> {
    let (compliance_unit, logic_verifiers) = run_job_pair(
        prover.scheduling(),
        async {
            prove_compliance(prover, compliance_witness)
                .await
                .context("prove compliance")
        },
        prove_logic_pair(prover, proving_key, verifying_key, consumed, created),
    )
    .await?;
    arm::action::new(compliance_unit, logic_verifiers)
        .map_err(|e| anyhow!("build the action from its proofs: {e:?}"))
}

/// Prove the consumed and the created resource's logic under one guest, and
/// return the verifiers in canonical tag order (consumed, then created), the
/// order the aggregation guest enforces.
async fn prove_logic_pair<T: Serialize + Send + 'static>(
    prover: &Prover,
    proving_key: &'static [u8],
    verifying_key: &Digest,
    consumed: T,
    created: T,
) -> Result<Vec<LogicVerifier>> {
    let jobs = [(consumed, "consumed"), (created, "created")].map(|(witness, label)| async move {
        let (proof, instance) = prove_logic(prover, proving_key, verifying_key, witness)
            .await
            .with_context(|| format!("prove the {label} resource's logic"))?;
        Ok(LogicVerifier {
            proof,
            instance,
            verifying_key: *verifying_key,
        })
    });
    run_jobs(prover.scheduling(), jobs).await
}

/// Wrap proven actions into a balanced, delta-proved `Transaction`. The delta
/// witness composes every action's `rcv`.
fn assemble_transaction(actions: Vec<Action>, rcvs: &[Vec<u8>]) -> Result<Transaction> {
    let delta_witness = arm::delta_proof::from_bytes_vec(rcvs)
        .map_err(|e| anyhow!("build delta witness: {e:?}"))?;

    let tx = Transaction::create(actions, Delta::Witness(delta_witness));
    let balanced_tx = arm::transaction::generate_delta_proof(tx)
        .map_err(|e| anyhow!("generate delta proof: {e:?}"))?;
    verify_tx(&balanced_tx)?;
    Ok(balanced_tx)
}

/// Verify a transaction's proofs and delta against the loaded kind table.
fn verify_tx(tx: &Transaction) -> Result<()> {
    let kind_table_commitment =
        *kind_table_hash().ok_or_else(|| anyhow!("kind table not loaded"))?;
    arm::transaction::verify(tx, kind_table_commitment, JournalEncoding::Risc0Serde)
        .map_err(|e| anyhow!("verify tx: {e:?}"))
}

/// Prove a one-action transaction (the shape every single-action fixture uses).
async fn prove_single_action_transaction(
    prover: &Prover,
    compliance_witness: ComplianceWitness,
    consumed_app_data: AppData,
) -> Result<Transaction> {
    let action = prove_action(
        prover,
        &compliance_witness,
        consumed_app_data,
        AppData::default(),
    )
    .await?;
    assemble_transaction(vec![action], std::slice::from_ref(&compliance_witness.rcv))
}

/// Shape constants for the transfer-shape fixture, anchored to the real
/// mainnet AnomaPay shielded transfer captured in May 2026 (commit e17e969,
/// `anomapay_transfer_0e345103.json`): 3 compliance units and 3,020 wire
/// bytes. That capture was the suite's heap-exhaustion (OOM) regression;
/// its proving inputs lived outside this repo and the pipeline that made it
/// is frozen, so this synthetic reproduction of its shape replaces it.
const TRANSFER_SHAPE_ACTIONS: usize = 3;
const CAPTURED_TRANSFER_WIRE_BYTES: usize = 3020;
/// Per created resource: an encrypted-note-sized resource payload and a
/// discovery payload, both with deletion criterion "never" so they are
/// emitted as events at settlement (the indexer-facing path the real
/// transfer exercised).
///
/// Sizing: the adapter's intake ceiling is the TxData account, created in a
/// single CPI, which Solana caps at 10,240 bytes of allocation — so the
/// largest settleable transaction is TxData's capacity (10,240 minus its
/// 89-byte header). The fixture sits just under that ceiling: 512 + 192
/// words = 2,816 payload bytes per created resource, 8,448 across the
/// three, for roughly 9.9 KiB of wire. The heap-budget test observes
/// whether settling a maximum-size transaction exceeds the default 32 KiB
/// BPF heap (the property the captured transfer's OOM had under v1).
const TRANSFER_SHAPE_RESOURCE_PAYLOAD_WORDS: usize = 512;
const TRANSFER_SHAPE_DISCOVERY_PAYLOAD_WORDS: usize = 192;

/// Deterministic payload blob: `words` u32 words derived from the action
/// index, deletion criterion "never" (emitted as an event at settlement).
fn transfer_shape_payload_blob(action_idx: usize, words: usize, salt: u32) -> ExpirableBlob {
    ExpirableBlob {
        blob: (0..words as u32)
            .map(|w| (action_idx as u32) << 16 | salt << 8 | (w & 0xff))
            .collect(),
        deletion_criterion: solana_pa::state::DELETION_CRITERION_NEVER,
    }
}

/// Generate the multi-action transfer-shape transaction: three single-unit
/// actions (the fixture's nonces 0..=2, distinct rcvs so the delta points
/// differ, as with production's random rcvs), each created resource
/// carrying event-emitted payload blobs. No external calls — the real
/// transfer had none.
async fn generate_transfer_shape_transaction(
    prover: &Prover,
    fixture_name: &str,
) -> Result<Transaction> {
    let mut witnesses = Vec::with_capacity(TRANSFER_SHAPE_ACTIONS);
    for i in 0..TRANSFER_SHAPE_ACTIONS {
        let (consumed_resource, nf_key, _, created_resource) =
            deterministic_ephemeral_resource(fixture_name, i as u32)?;

        // Distinct rcv per action: identical rcvs (with identical kinds and
        // quantities) would collapse the actions' delta points onto one
        // point, which is not the shape production transactions have.
        let rcv = Scalar::from((i + 1) as u64);
        let witness = single_action_compliance_witness(
            consumed_resource,
            MerklePath::empty(),
            nf_key,
            created_resource,
            rcv,
        );

        let created_app_data = AppData {
            resource_payload: vec![transfer_shape_payload_blob(
                i,
                TRANSFER_SHAPE_RESOURCE_PAYLOAD_WORDS,
                1,
            )],
            discovery_payload: vec![transfer_shape_payload_blob(
                i,
                TRANSFER_SHAPE_DISCOVERY_PAYLOAD_WORDS,
                2,
            )],
            ..AppData::default()
        };

        witnesses.push((witness, created_app_data));
    }

    let actions = run_jobs(
        prover.scheduling(),
        witnesses.iter().map(|(witness, created_app_data)| {
            prove_action(
                prover,
                witness,
                AppData::default(),
                created_app_data.clone(),
            )
        }),
    )
    .await?;
    let rcvs: Vec<Vec<u8>> = witnesses
        .into_iter()
        .map(|(witness, _)| witness.rcv)
        .collect();
    assemble_transaction(actions, &rcvs)
}

/// The transfer-shape fixture must be at least as large on the wire as the
/// captured mainnet transfer it replaces, or the OOM-regression coverage is
/// weaker than the real transaction it stands in for. Checked against the
/// final fixture bytes (aggregated, seal-encoded), which is the
/// representation the capture's 3,020 bytes measured.
fn check_transfer_shape_wire_size(tx_b64: &str) -> Result<()> {
    let wire_bytes = BASE64
        .decode(tx_b64)
        .context("decode transfer-shape fixture bytes")?
        .len();
    if wire_bytes < CAPTURED_TRANSFER_WIRE_BYTES {
        bail!(
            "transfer-shape fixture is {wire_bytes} wire bytes, smaller than the \
             {CAPTURED_TRANSFER_WIRE_BYTES}-byte captured mainnet transfer it replaces — \
             increase the payload sizes"
        );
    }
    Ok(())
}

async fn generate_test_transaction_with_external_payload(
    prover: &Prover,
    forwarder_mode: ForwarderMode,
    fixture_name: &str,
) -> Result<Transaction> {
    let (consumed_resource, nf_key, _, created_resource) =
        deterministic_ephemeral_resource(fixture_name, 0)?;
    // The consumed resource is ephemeral, so it needs no inclusion proof.
    let compliance_witness = single_action_compliance_witness(
        consumed_resource,
        MerklePath::empty(),
        nf_key,
        created_resource,
        Scalar::ONE,
    );

    // Bind the external payload into the consumed resource's app_data via
    // the passthrough logic circuit, which commits whatever it is given.
    let mut consumed_app_data = AppData::default();
    let external_blob = match &forwarder_mode {
        ForwarderMode::BlockTimeForwarder {
            output_mismatch, ..
        } => block_time_forwarder_external_payload_blob(*output_mismatch)?,
        ForwarderMode::TestForwarderFail => test_forwarder_fail_payload_blob()?,
        ForwarderMode::TestForwarderSilent => test_forwarder_silent_payload_blob()?,
        ForwarderMode::TestForwarderRelay => test_forwarder_relay_payload_blob()?,
    };
    consumed_app_data.external_payload.push(external_blob);
    if let ForwarderMode::BlockTimeForwarder {
        multi_external_call: true,
        ..
    } = forwarder_mode
    {
        consumed_app_data
            .external_payload
            .push(block_time_forwarder_external_payload_blob(false)?);
    }

    prove_single_action_transaction(prover, compliance_witness, consumed_app_data).await
}

/// A one-action transaction that consumes a zero-quantity ephemeral
/// passthrough resource and creates nothing, so its settlement appends no
/// commitment. The zero quantity keeps the action balanced with no created
/// resource to offset it.
async fn generate_consume_only_transaction(
    prover: &Prover,
    fixture_name: &str,
) -> Result<Transaction> {
    let (mut consumed, nf_key, _, _) = deterministic_ephemeral_resource(fixture_name, 0)?;
    consumed.quantity = 0;
    let consumed_nf = consumed
        .nullifier(&nf_key)
        .map_err(|e| anyhow!("compute consumed nullifier: {e:?}"))?;
    let compliance_witness = ComplianceWitness::from_parts(
        vec![ConsumedResourceWitness {
            resource: consumed,
            cm_merkle_path: MerklePath::empty(),
            nf_key,
        }],
        vec![],
        INITIAL_ROOT,
        &Scalar::ONE.to_bytes(),
        kind_table().to_vec(),
    );
    let root = ActionTree::new(vec![consumed_nf])
        .root()
        .map_err(|e| anyhow!("compute action tree root: {e:?}"))?;
    let passthrough_vk = Digest::new(PASSTHROUGH_LOGIC_GUEST_ID);
    let consumed_instance = LogicInstance {
        tag: consumed_nf,
        is_consumed: true,
        root,
        app_data: AppData::default(),
    };

    let (compliance_unit, (proof, instance)) = run_job_pair(
        prover.scheduling(),
        async {
            prove_compliance(prover, &compliance_witness)
                .await
                .context("prove compliance")
        },
        async {
            prove_logic(
                prover,
                PASSTHROUGH_LOGIC_GUEST_ELF,
                &passthrough_vk,
                consumed_instance,
            )
            .await
            .context("prove the consumed resource's logic")
        },
    )
    .await?;
    let action = arm::action::new(
        compliance_unit,
        vec![LogicVerifier {
            proof,
            instance,
            verifying_key: passthrough_vk,
        }],
    )
    .map_err(|e| anyhow!("build the action from its proofs: {e:?}"))?;
    assemble_transaction(vec![action], std::slice::from_ref(&compliance_witness.rcv))
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
            spl_forwarder: None,
            tx_b64: BASE64.encode(tx_bytes),
            tx_tampered_b64: String::new(),
            consumed_nullifiers_b64: nullifiers_b64.to_vec(),
            created_commitments_b64: Vec::new(),
            historical_roots_b64: Vec::new(),
        };

        let out_path = out_dir.join(file_name);
        fs::write(&out_path, serde_json::to_vec_pretty(&fixture)?)
            .with_context(|| format!("write error variant fixture to {}", out_path.display()))?;
        Ok(())
    };

    {
        let mut wrong_root = tx.clone();
        let instance = &mut require_aggregation_mut(&mut wrong_root)?.instance;
        let consumed = instance
            .actions
            .get_mut(0)
            .and_then(|a| a.consumed_publics.get_mut(0))
            .ok_or_else(|| anyhow!("aggregation instance has no consumed resources"))?;
        consumed.commitment_tree_root = Digest::from_bytes([1u8; 32]);
        write_variant("wrong_root.json", &wrong_root)?;
    }

    {
        let mut agg_variant = tx.clone();
        agg_variant.aggregation = None;
        write_variant("no_aggregation.json", &agg_variant)?;
    }

    {
        let mut garbage = tx.clone();
        require_aggregation_mut(&mut garbage)?.proof = vec![0xDE; 64];
        write_variant("garbage_proof.json", &garbage)?;
    }

    {
        // A well-formed seal under the fixture's own selector whose proof
        // point pi_c is corrupted: it decodes and routes to the verifier,
        // which must reject it. pi_c[0] lies in the claim digest a mock seal
        // carries, so the mock verifier rejects it as the Groth16 one does.
        let mut corrupt_seal = tx.clone();
        let aggregation = require_aggregation_mut(&mut corrupt_seal)?;
        let mut seal = Seal::try_from_slice(&aggregation.proof)
            .context("decode Seal from aggregation proof bytes")?;
        seal.proof.pi_c[0] ^= 0xff;
        aggregation.proof = borsh::to_vec(&seal).context("serialize corrupted Seal")?;
        write_variant("corrupt_seal.json", &corrupt_seal)?;
    }

    {
        let mut zero_action = tx.clone();
        require_aggregation_mut(&mut zero_action)?.instance.actions = Vec::new();
        write_variant("zero_action.json", &zero_action)?;
    }

    {
        // A WELL-FORMED transaction carrying Delta::Witness instead of the
        // proof (the actual witness the fixture was signed with). It
        // deserializes cleanly on-chain, so the PA must reject it with its
        // own witness check — never by crashing while deserializing the
        // scalar (the k256 stack-overflow class the v2 core split removed).
        let mut witness_delta = tx.clone();
        witness_delta.delta_proof = Delta::Witness(
            arm::delta_proof::DeltaWitness::from_bytes(&Scalar::ONE.to_bytes())
                .map_err(|e| anyhow!("build witness-delta variant: {e:?}"))?,
        );
        write_variant("witness_delta.json", &witness_delta)?;
    }

    Ok(())
}

/// Every consumed resource of a transaction's aggregation instance, in
/// action order: the single definition of that traversal.
fn consumed_publics(tx: &Transaction) -> Result<impl Iterator<Item = &ConsumedResourceAggregated>> {
    Ok(require_aggregation(tx)?
        .instance
        .actions
        .iter()
        .flat_map(|action| &action.consumed_publics))
}

fn consumed_nullifiers_b64(tx: &Transaction) -> Result<Vec<String>> {
    Ok(consumed_publics(tx)?
        .map(|c| BASE64.encode(c.resource_nullifier.as_bytes()))
        .collect())
}

/// The created commitments of an aggregated transaction in instance order:
/// the leaves its settlement appends.
fn created_commitments(tx: &Transaction) -> Result<Vec<Digest>> {
    Ok(require_aggregation(tx)?
        .instance
        .actions
        .iter()
        .flat_map(|action| &action.created_publics)
        .map(|c| c.resource_commitment)
        .collect())
}

fn created_commitments_b64(tx: &Transaction) -> Result<Vec<String>> {
    Ok(created_commitments(tx)?
        .iter()
        .map(|c| BASE64.encode(c.as_bytes()))
        .collect())
}

fn historical_roots(tx: &Transaction) -> Result<Vec<[u8; 32]>> {
    let roots: BTreeSet<[u8; 32]> = consumed_publics(tx)?
        .filter(|c| c.commitment_tree_root != INITIAL_ROOT)
        .map(|c| <[u8; 32]>::from(c.commitment_tree_root))
        .collect();
    Ok(roots.into_iter().collect())
}

/// Fields every fixture derives from a seal-encoded transaction: the
/// serialized transaction and a serialized tampered clone (base64), its
/// consumed nullifiers and historical roots (base64), and the seal's
/// selector.
struct DerivedFixtureFields {
    tx_b64: String,
    tx_tampered_b64: String,
    consumed_nullifiers_b64: Vec<String>,
    created_commitments_b64: Vec<String>,
    historical_roots_b64: Vec<String>,
    selector: String,
}

fn derive_fixture_fields(tx: &Transaction) -> Result<DerivedFixtureFields> {
    let tx_bytes = bincode::serialize(tx).context("serialize tx")?;
    eprintln!("  {} bytes", tx_bytes.len());

    let mut tx_tampered = tx.clone();
    mutate_tag_keep_structure(&mut tx_tampered)?;
    let tampered_bytes = bincode::serialize(&tx_tampered).context("serialize tampered tx")?;

    let selector = extract_selector(tx).context("extract selector from proof")?;
    eprintln!("  selector: {selector}");

    Ok(DerivedFixtureFields {
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tampered_bytes),
        consumed_nullifiers_b64: consumed_nullifiers_b64(tx)?,
        created_commitments_b64: created_commitments_b64(tx)?,
        historical_roots_b64: historical_roots(tx)?
            .iter()
            .map(|root| BASE64.encode(root))
            .collect(),
        selector,
    })
}

/// Seal-encode the aggregation proof, serialize the transaction (and a
/// tampered clone), extract nullifiers/selector/historical roots, and write
/// the resulting `Fixture` JSON. Shared by the default `Generate` path and
/// the `historical-root` path so both fixtures follow the exact same
/// on-disk convention.
fn finalize_and_write_fixture(
    tx: &mut Transaction,
    out_path: &Path,
    spl_forwarder: Option<SplForwarderMetadata>,
) -> Result<Fixture> {
    // The receipt type decides the seal encoding: dev-mode (Fake) receipts
    // become mock seals for the localnet mock verifier, real Groth16
    // receipts go through arm's canonical seal encoding. The fixture is
    // labeled accordingly (aggregation_proof_type, selector).
    let proof_type = timed_phase("encode_seal", || {
        let receipt_bytes = require_aggregation(tx)?.proof.clone();
        let inner: InnerReceipt =
            bincode::deserialize(&receipt_bytes).context("decode aggregation receipt")?;
        let (seal, proof_type) = if let InnerReceipt::Fake(fake) = inner {
            eprintln!("  dev-mode receipt -> mock seal (selector 0xffffffff)");
            (encode_mock_seal(&fake, tx)?, "mock")
        } else {
            let seal = encode_seal(&receipt_bytes).map_err(|e| anyhow!("encode seal: {e:?}"))?;
            (seal, "groth16")
        };
        require_aggregation_mut(tx)?.proof = seal;
        Ok(proof_type)
    })?;

    let fields = timed_phase("derive_fixture_fields", || derive_fixture_fields(tx))?;
    let fixture = Fixture {
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: proof_type,
        selector: fields.selector,
        spl_forwarder,
        tx_b64: fields.tx_b64,
        tx_tampered_b64: fields.tx_tampered_b64,
        consumed_nullifiers_b64: fields.consumed_nullifiers_b64,
        created_commitments_b64: fields.created_commitments_b64,
        historical_roots_b64: fields.historical_roots_b64,
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

/// The one commitment an existing fixture settles: leaf 0 of the tree the
/// historical-root pair is proven over, when that fixture is
/// `batch_groth16.json`.
fn read_sole_created_commitment(path: &Path) -> Result<Digest> {
    let leaves = created_commitments(&load_fixture_tx(path)?)?;
    if leaves.len() != 1 {
        bail!(
            "{} must settle exactly one commitment to serve as the known single-leaf tree \
             base for historical-root fixture generation (found {})",
            path.display(),
            leaves.len()
        );
    }
    Ok(leaves[0])
}

/// Build the compliance witness for the historical-root *committer*
/// transaction: consumes a fresh ephemeral resource as usual, but its
/// created resource is genuinely non-ephemeral (`is_ephemeral: false`), so a
/// later transaction can consume it through a real Merkle-inclusion proof
/// rather than the ephemeral-root shortcut. Returns the witness plus the
/// created resource and the nullifier key that unlocks it, both needed to
/// build the consumer transaction afterward. The consumed resource carries
/// the committer fixture's first nonce.
fn build_historical_root_committer_witness(
    committer_name: &str,
) -> Result<(ComplianceWitness, Resource, NullifierKey)> {
    let (consumed_resource, nf_key, _, mut created_resource) =
        deterministic_ephemeral_resource(committer_name, 0)?;
    created_resource.is_ephemeral = false;

    let compliance_witness = single_action_compliance_witness(
        consumed_resource,
        MerklePath::empty(),
        nf_key.clone(),
        created_resource,
        Scalar::ONE,
    );

    Ok((compliance_witness, created_resource, nf_key))
}

/// Build the compliance witness for the historical-root *consumer*
/// transaction: genuinely consumes `committed_resource` (is_ephemeral:
/// false) via `merkle_path`, which must reconstruct the real on-chain root
/// the committer's settlement produced.
fn build_historical_root_consumer_witness(
    committed_resource: Resource,
    committer_nf_key: NullifierKey,
    merkle_path: MerklePath,
) -> Result<ComplianceWitness> {
    let passthrough_vk = Digest::new(PASSTHROUGH_LOGIC_GUEST_ID);

    let consumed_nf = committed_resource
        .nullifier(&committer_nf_key)
        .map_err(|e| anyhow!("compute consumer's consumed nullifier: {e:?}"))?;

    let output_nf_key = NullifierKey::default();
    let created_resource = Resource {
        logic_ref: passthrough_vk,
        nk_commitment: output_nf_key.commit(),
        quantity: 1,
        is_ephemeral: true,
        nonce: first_created_nonce(consumed_nf)?,
        ..Default::default()
    };

    Ok(single_action_compliance_witness(
        committed_resource,
        merkle_path,
        committer_nf_key,
        created_resource,
        Scalar::ONE,
    ))
}

/// Prove a one-action historical-root transaction (`label` names it in the
/// log), aggregate it, and verify the aggregation.
async fn prove_aggregated_single_action(
    prover: &Prover,
    label: &str,
    witness: ComplianceWitness,
) -> Result<Transaction> {
    eprintln!("phase: generate historical-root {label} transaction");
    let start = Instant::now();
    let tx = prove_single_action_transaction(prover, witness, AppData::default())
        .await
        .with_context(|| format!("build {label} transaction"))?;
    eprintln!(
        "phase done: {label} transaction ({})",
        fmt_duration(start.elapsed())
    );

    eprintln!("phase: aggregate {label} transaction (batch, groth16)");
    let start = Instant::now();
    let tx = aggregate_tx(prover, tx)
        .await
        .with_context(|| format!("aggregate {label} tx"))?;
    eprintln!(
        "phase done: aggregate {label} ({})",
        fmt_duration(start.elapsed())
    );
    arm::transaction::verify_aggregation(&tx, JournalEncoding::Risc0Serde)
        .map_err(|e| anyhow!("verify {label} aggregated proof: {e:?}"))?;
    Ok(tx)
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
/// `MerklePath::root()` (arm's own hash) before any proof is generated, so
/// a divergence between the two hash implementations fails loudly instead of
/// producing a fixture that can never settle.
async fn generate_historical_root_fixtures(
    batch_groth16_path: &Path,
    committer_out: &Path,
    consumer_out: &Path,
    prover_choice: Option<ProverChoice>,
) -> Result<()> {
    let prover = resolve_prover(prover_choice)?;

    let batch_groth16_leaf = read_sole_created_commitment(batch_groth16_path)
        .context("read batch_groth16.json's committed leaf")?;

    let (committer_witness, committed_resource, committer_nf_key) =
        build_historical_root_committer_witness(fixture_name(committer_out)?)?;
    // The committer settles right after batch_groth16 (leaf 0), as leaf 1.
    // Its commitment is known before it is proven, so the consumer's Merkle
    // path is too, and the two transactions prove independently.
    let committed_cm = committed_resource.commitment();
    let (merkle_path, expected_root) =
        checked_pa_merkle_path(&[batch_groth16_leaf, committed_cm], 1)?;
    let consumer_witness =
        build_historical_root_consumer_witness(committed_resource, committer_nf_key, merkle_path)
            .context("build consumer witness")?;

    let (mut committer_tx, mut consumer_tx) = run_job_pair(
        prover.scheduling(),
        prove_aggregated_single_action(&prover, "committer", committer_witness),
        prove_aggregated_single_action(&prover, "consumer", consumer_witness),
    )
    .await?;

    finalize_and_write_fixture(&mut committer_tx, committer_out, None)
        .context("write committer fixture")?;

    // The whole point of this fixture: the consumed root must be a genuine,
    // non-padding historical root. If it were the initial root, is_root_valid
    // would accept it unconditionally before the marker lookup ever runs,
    // exactly the coverage gap this fixture exists to close.
    let consumer_root = consumed_publics(&consumer_tx)?
        .next()
        .map(|c| c.commitment_tree_root)
        .ok_or_else(|| anyhow!("consumer tx has no consumed resources"))?;
    if consumer_root == INITIAL_ROOT {
        bail!(
            "consumer's consumed commitment tree root is the initial root -- this fixture \
             would prove nothing about historical root retention"
        );
    }
    if consumer_root != expected_root {
        bail!(
            "consumer's consumed commitment tree root ({}) does not match the expected \
             historical root ({})",
            hex::encode(consumer_root.as_bytes()),
            hex::encode(expected_root.as_bytes())
        );
    }
    eprintln!(
        "confirmed: consumer's consumed commitment tree root = {} (non-padding, matches the \
         committer's post-settlement root)",
        hex::encode(consumer_root.as_bytes())
    );

    finalize_and_write_fixture(&mut consumer_tx, consumer_out, None)
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

/// Read a fixture JSON and bincode-decode its transaction.
fn load_fixture_tx(path: &Path) -> Result<Transaction> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).context("parsing fixture JSON")?;
    let tx_b64 = map
        .get("tx_b64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing tx_b64 field in {}", path.display()))?;
    let tx_bytes = BASE64.decode(tx_b64).context("decoding tx_b64")?;
    bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")
}

fn dump_fixture(input: &Path) -> Result<()> {
    let tx = load_fixture_tx(input)?;

    eprintln!("Transaction:");
    match &tx.aggregation {
        Some(aggregation) => {
            let instance = &aggregation.instance;
            eprintln!("  aggregation instance:");
            eprintln!(
                "    compliance_key: {}",
                hex::encode(&instance.compliance_key.as_bytes()[..4])
            );
            eprintln!(
                "    kind_table_commitment: {}",
                hex::encode(&instance.kind_table_commitment.as_bytes()[..4])
            );
            eprintln!("    actions: {}", instance.actions.len());
            for (ai, action) in instance.actions.iter().enumerate() {
                eprintln!("    Action {}:", ai);
                for (ci, consumed) in action.consumed_publics.iter().enumerate() {
                    let nf = consumed.resource_nullifier.as_bytes();
                    eprintln!(
                        "      consumed {}: nullifier={:02x}{:02x}..., external_payload={}",
                        ci,
                        nf[0],
                        nf[1],
                        consumed.app_data.external_payload.len()
                    );
                }
                for (ci, created) in action.created_publics.iter().enumerate() {
                    let cm = created.resource_commitment.as_bytes();
                    eprintln!(
                        "      created {}: commitment={:02x}{:02x}..., external_payload={}",
                        ci,
                        cm[0],
                        cm[1],
                        created.app_data.external_payload.len()
                    );
                }
            }
            eprintln!("  aggregation proof: {} bytes", aggregation.proof.len());
        }
        None => {
            let actions = tx.actions.as_deref().unwrap_or(&[]);
            eprintln!("  actions (unaggregated): {}", actions.len());
            for (ai, action) in actions.iter().enumerate() {
                eprintln!(
                    "  Action {}: logic_verifier_inputs: {}",
                    ai,
                    action.logic_verifier_inputs.len()
                );
            }
        }
    }
    eprintln!(
        "  delta_proof: {:?}",
        std::mem::discriminant(&tx.delta_proof)
    );
    Ok(())
}

/// Enter mock mode (no-op unless `mock`): proofs run through the local
/// dev-mode executor (guests execute, nothing is proven), and
/// `finalize_and_write_fixture` turns the resulting Fake aggregation receipt
/// into a mock seal. RISC0_DEV_MODE is a runtime env var read by risc0 at
/// proving time; setting it here keeps the flag self-contained instead of
/// depending on ambient environment state.
fn apply_mock_mode(&ProverArgs { mock, prover }: &ProverArgs) -> Option<ProverChoice> {
    if !mock {
        return prover;
    }
    env::set_var("RISC0_DEV_MODE", "1");
    eprintln!("mode: mock (dev-mode receipts -> mock seal, selector 0xffffffff)");
    Some(ProverChoice::Local)
}

/// Resolve the `Prover` to use: an explicit `--prover` wins; otherwise default
/// to `Queue` when `QUEUE_BASE_URL` is set (preserving today's behavior when a
/// queue is configured), `Local` otherwise (so fixture-gen works out of the
/// box with no queue credentials). Logs the resolved prover.
fn resolve_prover(choice: Option<ProverChoice>) -> Result<Prover> {
    let choice = choice.unwrap_or_else(|| {
        if env::var("QUEUE_BASE_URL").is_ok() {
            ProverChoice::Queue
        } else {
            ProverChoice::Local
        }
    });
    let prover = match choice {
        ProverChoice::Local => Prover::Local,
        ProverChoice::Queue => Prover::Queue(build_queue_client()?),
    };
    match &prover {
        Prover::Local => eprintln!("prover: local (CPU risc0 prover)"),
        Prover::Queue(_) => eprintln!(
            "prover: queue ({})",
            env::var("QUEUE_BASE_URL").unwrap_or_default()
        ),
    }
    Ok(prover)
}

/// Load the kind table every proving and verification path checks witness
/// kind tables against.
fn load_kind_table(path: &Path) -> Result<()> {
    init_kind_table_from_file(path)
        .map_err(|e| anyhow!("load kind table {}: {e:?}", path.display()))?;
    eprintln!(
        "kind table: {} (commitment {})",
        path.display(),
        hex::encode(
            kind_table_hash()
                .ok_or_else(|| anyhow!("kind table not loaded"))?
                .as_bytes()
        )
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Dump { input } => {
            load_kind_table(Path::new(KIND_TABLE_PATH))?;
            dump_fixture(&input)
        }
        Command::HistoricalRoot {
            batch_groth16,
            committer_out,
            consumer_out,
            prover,
        } => {
            load_kind_table(Path::new(KIND_TABLE_PATH))?;
            generate_historical_root_fixtures(
                &batch_groth16,
                &committer_out,
                &consumer_out,
                apply_mock_mode(&prover),
            )
            .await
        }
        Command::Generate(shape) => generate_fixture(shape).await,
    }
}

async fn generate_fixture(shape: ShapeCommand) -> Result<()> {
    let (ShapeCommand::Batch { generate, .. }
    | ShapeCommand::OutputMismatch { generate }
    | ShapeCommand::ForwarderFail { generate }
    | ShapeCommand::ForwarderSilent { generate }
    | ShapeCommand::ForwarderRelay { generate }
    | ShapeCommand::ConsumeOnly { generate }
    | ShapeCommand::TransferShape { generate }
    | ShapeCommand::SplTokenWrap { generate, .. }
    | ShapeCommand::SplTokenUnwrap { generate, .. }) = &shape;
    let GenerateArgs {
        out_path,
        prover,
        error_variants,
        kind_table,
        debug_assumptions,
    } = generate;
    load_kind_table(kind_table)?;
    let total_start = Instant::now();

    let prover = resolve_prover(apply_mock_mode(prover))?;

    eprintln!("fixture output: {}", out_path.display());
    eprintln!("mode: aggregated (batch Groth16)");
    match &shape {
        ShapeCommand::Batch {
            multi_external_call: true,
            ..
        } => eprintln!("mode: multi-external-call (two external payload blobs)"),
        ShapeCommand::OutputMismatch { .. } => eprintln!(
            "mode: output-mismatch (intentionally wrong expected_output for ExternalCallOutputMismatch test)"
        ),
        ShapeCommand::ConsumeOnly { .. } => {
            eprintln!("mode: consume-only (one zero-quantity consumed resource, nothing created)")
        }
        ShapeCommand::TransferShape { .. } => eprintln!(
            "mode: transfer-shape ({TRANSFER_SHAPE_ACTIONS} actions, event-emitted payloads, no external calls)"
        ),
        ShapeCommand::SplTokenWrap { wrap_nonce, .. } => eprintln!(
            "mode: AnomaPay wrap (transfer logic {}, forwarder nonce {wrap_nonce})",
            TOKEN_TRANSFER_ID
        ),
        ShapeCommand::SplTokenUnwrap { wrap, .. } => eprintln!(
            "mode: AnomaPay unwrap (transfer logic {}, wrap fixture {})",
            TOKEN_TRANSFER_ID,
            wrap.display()
        ),
        ShapeCommand::Batch { .. }
        | ShapeCommand::ForwarderFail { .. }
        | ShapeCommand::ForwarderSilent { .. }
        | ShapeCommand::ForwarderRelay { .. } => {}
    }
    if let Some(dir) = &error_variants {
        eprintln!("error variants output dir: {}", dir.display());
    }

    eprintln!("phase: generate_test_transaction");
    let gen_start = Instant::now();
    let name = fixture_name(out_path)?;
    let single_action = |mode| generate_test_transaction_with_external_payload(&prover, mode, name);
    let (mut tx, spl_forwarder) = match &shape {
        ShapeCommand::Batch {
            multi_external_call,
            ..
        } => (
            single_action(ForwarderMode::BlockTimeForwarder {
                output_mismatch: false,
                multi_external_call: *multi_external_call,
            })
            .await?,
            None,
        ),
        ShapeCommand::OutputMismatch { .. } => (
            single_action(ForwarderMode::BlockTimeForwarder {
                output_mismatch: true,
                multi_external_call: false,
            })
            .await?,
            None,
        ),
        ShapeCommand::ForwarderFail { .. } => {
            (single_action(ForwarderMode::TestForwarderFail).await?, None)
        }
        ShapeCommand::ForwarderSilent { .. } => (
            single_action(ForwarderMode::TestForwarderSilent).await?,
            None,
        ),
        ShapeCommand::ForwarderRelay { .. } => (
            single_action(ForwarderMode::TestForwarderRelay).await?,
            None,
        ),
        ShapeCommand::ConsumeOnly { .. } => (
            generate_consume_only_transaction(&prover, name).await?,
            None,
        ),
        ShapeCommand::TransferShape { .. } => (
            generate_transfer_shape_transaction(&prover, name).await?,
            None,
        ),
        ShapeCommand::SplTokenWrap { wrap_nonce, .. } => {
            let (tx, metadata) =
                generate_anomapay_wrap_transaction(&prover, name, *wrap_nonce).await?;
            (tx, Some(metadata))
        }
        ShapeCommand::SplTokenUnwrap {
            wrap, to_escrow, ..
        } => {
            let leaves = created_commitments(&load_fixture_tx(wrap)?)?;
            let (tx, metadata) = generate_anomapay_unwrap_transaction(
                &prover,
                fixture_name(wrap)?,
                &leaves,
                *to_escrow,
            )
            .await?;
            (tx, Some(metadata))
        }
    };
    eprintln!(
        "phase done: generate_test_transaction ({})",
        fmt_duration(gen_start.elapsed())
    );

    if *debug_assumptions {
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
        arm::transaction::verify_aggregation(&tx, JournalEncoding::Risc0Serde)
            .map_err(|e| anyhow!("verify aggregated proof: {e:?}"))
    })?;

    let fixture = finalize_and_write_fixture(&mut tx, out_path, spl_forwarder)?;

    if matches!(shape, ShapeCommand::TransferShape { .. }) {
        check_transfer_shape_wire_size(&fixture.tx_b64)?;
    }

    if let Some(dir) = error_variants.as_deref() {
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
    let expected_claim = ReceiptClaim::ok(*vk, MaybePruned::Pruned(journal_digest));
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

    let compliance_receipts = arm::transaction::get_compliance_inner_receipts(tx)
        .map_err(|e| anyhow!("decode compliance receipts: {e:?}"))?;
    let compliance_journals = arm::transaction::get_compliance_instances(tx);
    for (idx, (inner, journal)) in compliance_receipts
        .into_iter()
        .zip(compliance_journals)
        .enumerate()
    {
        let receipt = Receipt::new(inner, journal.clone());
        let receipt_claim_digest = receipt.claim().context("read compliance claim")?.digest();
        let expected_claim_digest = compute_expected_claim_digest(&journal, &COMPLIANCE_VK);
        eprintln!(
            "debug_assumptions: compliance[{idx}] receipt_claim_digest={} expected_claim_digest={}",
            receipt_claim_digest, expected_claim_digest
        );
    }

    let logic_verifiers = arm::transaction::get_logic_verifiers(tx)
        .map_err(|e| anyhow!("reconstruct logic verifiers: {e:?}"))?;
    for (idx, verifier) in logic_verifiers.iter().enumerate() {
        let inner: InnerReceipt =
            bincode::deserialize(&verifier.proof).context("decode logic InnerReceipt")?;
        let receipt = Receipt::new(inner, verifier.instance.clone());
        let receipt_claim_digest = receipt.claim().context("read logic claim")?.digest();
        let expected_claim_digest =
            compute_expected_claim_digest(&verifier.instance, &verifier.verifying_key);
        eprintln!(
            "debug_assumptions: logic[{idx}] instance_len={} (mod4={}) receipt_claim_digest={} expected_claim_digest={}",
            verifier.instance.len(),
            verifier.instance.len() % 4,
            receipt_claim_digest,
            expected_claim_digest
        );
    }

    eprintln!("debug_assumptions: end");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arm::compliance::ComplianceInstance;
    // Tests build transactions via the local arm prover directly (not through
    // `Prover::Local`/`spawn_blocking`) so they stay synchronous. Requires
    // `RISC0_DEV_MODE=1` to run.

    fn init_test_kind_table() {
        init_kind_table_from_file(Path::new(KIND_TABLE_PATH)).expect("load committed kind table");
    }

    fn synthetic_leaves(n: usize) -> Vec<Digest> {
        (0..n)
            .map(|i| Digest::from_bytes([i as u8 + 1; 32]))
            .collect()
    }

    /// Every leaf's path reconstructs the root the adapter's own
    /// `append_to_tree` produces, for trees below and above the growth points.
    #[test]
    fn pa_merkle_path_reconstructs_the_adapter_root_for_every_leaf() {
        for n in 1..=12 {
            let leaves = synthetic_leaves(n);
            for i in 0..n {
                checked_pa_merkle_path(&leaves, i)
                    .unwrap_or_else(|e| panic!("leaf {i} of {n}: {e}"));
            }
        }
    }

    /// The wrap creates the owner's resource, and an unwrap over a synthetic
    /// tree with that resource at the last leaf consumes it; both proven
    /// under the transfer logic in dev mode through the real guest.
    #[tokio::test(flavor = "multi_thread")]
    async fn anomapay_unwrap_consumes_the_resource_the_wrap_creates() {
        init_test_kind_table();
        let wrap_name = "spl_token_wrap";
        let (_, owner, wrap) = seeded_wrap(wrap_name).unwrap();
        let wrapped = wrap.created;

        let (tx, _) =
            generate_anomapay_wrap_transaction(&Prover::Local, wrap_name, ANOMAPAY_WRAP_NONCE)
                .await
                .unwrap();
        let action = &tx.actions.as_ref().unwrap()[0];
        assert_eq!(action.logic_verifier_inputs[1].tag, wrapped.commitment());
        for input in &action.logic_verifier_inputs {
            assert_eq!(input.verifying_key, TOKEN_TRANSFER_ID);
        }

        let mut leaves = synthetic_leaves(9);
        leaves.push(wrapped.commitment());
        let (tx, _) =
            generate_anomapay_unwrap_transaction(&Prover::Local, wrap_name, &leaves, false)
                .await
                .unwrap();
        let action = &tx.actions.as_ref().unwrap()[0];
        assert_eq!(
            action.logic_verifier_inputs[0].tag,
            wrapped.nullifier(&owner.keys.nf_key).unwrap(),
            "the unwrap consumes the wrapped resource"
        );
        assert_eq!(
            action.logic_verifier_inputs[0].verifying_key,
            TOKEN_TRANSFER_ID
        );
    }

    /// Tracks how many mock proving jobs are in flight, and the most ever.
    #[derive(Default)]
    struct InFlight {
        now: std::cell::Cell<usize>,
        max: std::cell::Cell<usize>,
    }

    /// A mock proving job returning `id`: it enters, yields `yields` times
    /// (so jobs given more yields finish later), and leaves.
    async fn mock_job(in_flight: &InFlight, id: usize, yields: usize) -> Result<usize> {
        in_flight.now.set(in_flight.now.get() + 1);
        in_flight
            .max
            .set(in_flight.max.get().max(in_flight.now.get()));
        for _ in 0..yields {
            tokio::task::yield_now().await;
        }
        in_flight.now.set(in_flight.now.get() - 1);
        Ok(id)
    }

    /// Queue jobs are all in flight together, and their results come back in
    /// input order even though the later jobs finish first.
    #[tokio::test]
    async fn concurrent_jobs_overlap_and_keep_input_order() {
        let in_flight = InFlight::default();
        let jobs = (0..3).map(|id| mock_job(&in_flight, id, 3 - id));
        let results = run_jobs(JobScheduling::Concurrent, jobs).await.unwrap();
        assert_eq!(results, vec![0, 1, 2], "results must keep input order");
        assert_eq!(in_flight.max.get(), 3, "all three jobs must run at once");

        let in_flight = InFlight::default();
        let pair = run_job_pair(
            JobScheduling::Concurrent,
            mock_job(&in_flight, 0, 2),
            mock_job(&in_flight, 1, 1),
        )
        .await
        .unwrap();
        assert_eq!(pair, (0, 1), "the pair must keep input order");
        assert_eq!(in_flight.max.get(), 2, "both jobs must run at once");
    }

    /// Local jobs run strictly one at a time, in input order.
    #[tokio::test]
    async fn sequential_jobs_never_overlap() {
        let in_flight = InFlight::default();
        let jobs = (0..3).map(|id| mock_job(&in_flight, id, 3 - id));
        let results = run_jobs(JobScheduling::Sequential, jobs).await.unwrap();
        assert_eq!(results, vec![0, 1, 2], "results must keep input order");
        assert_eq!(in_flight.max.get(), 1, "only one job may run at a time");

        let in_flight = InFlight::default();
        let pair = run_job_pair(
            JobScheduling::Sequential,
            mock_job(&in_flight, 0, 2),
            mock_job(&in_flight, 1, 1),
        )
        .await
        .unwrap();
        assert_eq!(pair, (0, 1), "the pair must keep input order");
        assert_eq!(in_flight.max.get(), 1, "only one job may run at a time");
    }

    /// The local prover schedules sequentially: proving in parallel on one
    /// machine exhausts it.
    #[test]
    fn local_prover_schedules_sequentially() {
        assert!(matches!(
            Prover::Local.scheduling(),
            JobScheduling::Sequential
        ));
    }

    /// Each fixture shape is its own subcommand, and options that belong to
    /// another shape are rejected by the parser.
    #[test]
    fn subcommands_accept_only_their_own_options() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(std::iter::once("fixture-gen").chain(args.iter().copied()))
        };
        assert!(parse(&["batch", "--multi-external-call", "out.json"]).is_ok());
        assert!(parse(&["spl-token-wrap", "--wrap-nonce", "2", "out.json"]).is_ok());
        assert!(parse(&["spl-token-unwrap", "--wrap", "a.json", "out.json"]).is_ok());
        for rejected in [
            &["spl-token-unwrap", "out.json"][..],
            &["forwarder-fail", "--multi-external-call", "out.json"],
            &["transfer-shape", "--multi-external-call", "out.json"],
            &["batch", "--wrap-nonce", "2", "out.json"],
            &["batch", "--wrap", "a.json", "out.json"],
            &["output-mismatch", "--multi-external-call", "out.json"],
            &[
                "spl-token-unwrap",
                "--wrap",
                "a.json",
                "--wrap",
                "b.json",
                "out.json",
            ],
            &["spl-token-wrap", "--multi-external-call", "out.json"],
            &["batch", "--mock", "--prover", "queue", "out.json"],
            &["batch", "--error-variants", "", "out.json"],
            &[
                "historical-root",
                "--kind-table",
                "t.json",
                "a.json",
                "b.json",
                "c.json",
            ],
            &["batch"],
        ] {
            assert!(parse(rejected).is_err(), "must reject {rejected:?}");
        }
    }

    fn fixture_nullifier(fixture_name: &str, index: u32) -> Digest {
        let (_, _, nullifier, _) = deterministic_ephemeral_resource(fixture_name, index).unwrap();
        nullifier
    }

    /// A fixture's nullifiers are a function of its name alone: the same name
    /// gives the same nullifier on every run, and different names (or
    /// different resource indices within one fixture) give different ones.
    #[test]
    fn fixture_nullifiers_derive_from_the_fixture_name() {
        assert_eq!(
            fixture_nullifier("batch_groth16", 0),
            fixture_nullifier("batch_groth16", 0),
            "the same fixture name must derive the same nullifier"
        );
        assert_ne!(
            fixture_nullifier("spl_token_wrap", 0),
            fixture_nullifier("spl_token_wrap_replay", 0),
            "differently named fixtures must not share a nullifier"
        );
        assert_ne!(
            fixture_nullifier("batch_groth16_transfer_shape", 0),
            fixture_nullifier("batch_groth16_transfer_shape", 1),
            "a fixture's resources must not share a nullifier"
        );
    }

    /// `ComplianceUnit::instance` is journal bytes on the wire — parse,
    /// mutate (via `f`), and re-encode in one place.
    fn mutate_compliance_instance<R>(
        cu: &mut ComplianceUnit,
        f: impl FnOnce(&mut ComplianceInstance) -> R,
    ) -> Result<R> {
        let mut inst: ComplianceInstance =
            arm::proving_system::journal_to_instance(&cu.instance)
                .map_err(|e| anyhow!("parse compliance instance: {e:?}"))?;
        let r = f(&mut inst);
        cu.instance = arm::proving_system::instance_to_journal(&inst)
            .map_err(|e| anyhow!("re-encode mutated compliance instance: {e:?}"))?;
        Ok(r)
    }

    /// Helper: build a minimal valid transaction with a real delta proof.
    /// Uses the passthrough logic circuit and ephemeral resources.
    fn build_valid_tx_with_delta_proof(fixture_name: &str) -> Transaction {
        init_test_kind_table();
        let (consumed, nf_key, consumed_nf, created) =
            deterministic_ephemeral_resource(fixture_name, 0).unwrap();
        let passthrough_vk = Digest::new(PASSTHROUGH_LOGIC_GUEST_ID);
        let created_cm = created.commitment();

        let witness = single_action_compliance_witness(
            consumed,
            MerklePath::empty(),
            nf_key,
            created,
            Scalar::ONE,
        );
        let cu = arm::compliance_unit::create(&witness, LocalProofType::Succinct).unwrap();

        let tags = vec![consumed_nf, created_cm];
        let root = ActionTree::new(tags).root().unwrap();

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
            proof: cp,
            instance: cj,
            verifying_key: passthrough_vk,
        };
        let created_logic = LogicVerifier {
            proof: crp,
            instance: crj,
            verifying_key: passthrough_vk,
        };

        let action = arm::action::new(cu, vec![consumed_logic, created_logic]).unwrap();

        let delta_witness =
            arm::delta_proof::from_bytes_vec(std::slice::from_ref(&witness.rcv)).unwrap();
        let tx = Transaction::create(vec![action], Delta::Witness(delta_witness));
        arm::transaction::generate_delta_proof(tx).unwrap()
    }

    /// A valid transaction must pass delta verification via the k256 path.
    #[test]
    fn valid_tx_passes_delta_verification() {
        let tx = build_valid_tx_with_delta_proof("valid_tx");
        verify_tx(&tx).unwrap();
    }

    /// Swapping the nullifier and commitment tags in the delta message
    /// must invalidate the delta proof signature.
    #[test]
    fn swapped_tags_invalidate_delta_proof() {
        let tx = build_valid_tx_with_delta_proof("swapped_tags");
        verify_tx(&tx).unwrap();

        // Swap nullifier and commitment in the compliance instance. This
        // changes the action tree root and therefore the delta message,
        // invalidating the signature over the original message hash.
        let mut swapped = tx.clone();
        let actions = swapped.actions.as_mut().unwrap();
        mutate_compliance_instance(&mut actions[0].compliance_unit, |inst| {
            std::mem::swap(
                &mut inst.consumed_publics[0].resource_nullifier,
                &mut inst.created_publics[0].resource_commitment,
            );
        })
        .unwrap();

        let result = verify_tx(&swapped);
        assert!(result.is_err(), "swapped nf/cm must invalidate delta proof");
    }

    /// Mutating a single delta coordinate must invalidate the delta proof.
    #[test]
    fn mutated_delta_x_invalidates_proof() {
        let mut tx = build_valid_tx_with_delta_proof("mutated_delta_x");
        verify_tx(&tx).unwrap();

        // Flip a word in delta_x
        let actions = tx.actions.as_mut().unwrap();
        mutate_compliance_instance(&mut actions[0].compliance_unit, |inst| {
            inst.delta_x[0] ^= 0xFFFFFFFF;
        })
        .unwrap();

        let result = verify_tx(&tx);
        assert!(
            result.is_err(),
            "mutated delta_x must invalidate delta proof"
        );
    }

    /// Mutating a nullifier must invalidate the delta proof (changes the message hash).
    #[test]
    fn mutated_nullifier_invalidates_delta_proof() {
        let mut tx = build_valid_tx_with_delta_proof("mutated_nullifier");
        verify_tx(&tx).unwrap();

        // Corrupt the nullifier
        let actions = tx.actions.as_mut().unwrap();
        mutate_compliance_instance(&mut actions[0].compliance_unit, |inst| {
            inst.consumed_publics[0].resource_nullifier = Digest::from_bytes([0xFF; 32]);
        })
        .unwrap();

        let result = verify_tx(&tx);
        assert!(
            result.is_err(),
            "mutated nullifier must invalidate delta proof"
        );
    }
}
