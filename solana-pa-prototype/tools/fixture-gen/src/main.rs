use anchor_lang::prelude::{borsh, AnchorDeserialize as BorshDeserialize, Pubkey};
use anoma_pa_solana_client::merkle::merkle_path;
use anoma_pa_solana_client::settlement_input::settlement_transaction;
use anoma_pa_solana_client::MOCK_SELECTOR;
use anyhow::{anyhow, bail, Context, Result};
use arm::action::Action;
use arm::action_tree::ActionTree;
use arm::aggregation_instance::ConsumedResourceAggregated;
use arm::compliance::{ComplianceWitness, INITIAL_ROOT};
use arm::compliance_unit::ComplianceUnit;
use arm::constants::{
    init_kind_table_from_file, kind_table, kind_table_hash, BATCH_AGGREGATION_PK, COMPLIANCE_PK,
    COMPLIANCE_VK,
};
use arm::logic_instance::ExpirableBlob;
use arm::logic_instance::{AppData, LogicInstance};
use arm::logic_proof::LogicVerifier;
use arm::merkle_path::MerklePath;
use arm::nullifier_key::NullifierKey;
use arm::proving_system::{JournalEncoding, ProofType as LocalProofType};
use arm::resource::{ConsumedResourceWitness, Resource};
use arm::transaction::{Aggregation, Delta, Transaction};
use arm::Digest;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use clap::{Args, Parser, Subcommand, ValueEnum};
use futures::future::try_join_all;
use heliax_ap_orchestrator_sdk::{
    AggregateProofResult, BaseProofResult, GpuAggregationProofPayload, GpuComplianceProofPayload,
    GpuLogicProofPayload, ProofPayload, ProofType as QueueProofType, QueueClient,
};
use k256::Scalar;
use risc0_zkvm::sha::{Digestible as _, Sha256 as _};
use risc0_zkvm::{InnerReceipt, MaybePruned, Receipt, ReceiptClaim};
use serde::{Deserialize, Serialize};
use solana_pa::verifier_router::types::Seal;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anoma_pa_testkit::fixtures::passthrough::{PASSTHROUGH_LOGIC_PK, PASSTHROUGH_LOGIC_VK};

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
use solana_pa::external_calls::encode_external_call;
use solana_pa::state::PAStateAccount;
use solana_pa::types::{OutputMode, SolanaExternalCall};
use test_forwarder::{MODE_EMPTY, MODE_FAIL, MODE_SILENT};

#[derive(Serialize)]
struct Fixture {
    /// The name the fixture's resource nonces derive from (see `fixture_name`).
    name: String,
    format: &'static str,
    aggregation_strategy: &'static str,
    aggregation_proof_type: &'static str,
    /// Groth16 verifier selector extracted from the proof's verifier_parameters.
    /// Format: "0x" + 4-byte hex (e.g., "0x73c457ba").
    selector: String,
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
    /// ForwarderCallOutputMismatch test.
    OutputMismatch {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder with a failing instruction.
    ForwarderFail {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder returning no data where the call expects an empty
    /// output.
    ForwarderSilent {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// The test forwarder returning the empty output the call expects.
    ForwarderEmptyOutput {
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
    /// One action that consumes an ephemeral resource and creates a
    /// non-ephemeral one, which a later transaction can only spend through
    /// a real Merkle path to a retained historical root.
    HistoricalRootCommitter {
        #[command(flatten)]
        generate: GenerateArgs,
    },
    /// Spend the committer's resource through its Merkle path to the root
    /// the committer's settlement produced on a fresh adapter.
    HistoricalRootConsumer {
        /// The committer fixture, the first settlement of a fresh adapter:
        /// its created commitments are the tree the consumer proves
        /// membership in.
        #[arg(long, value_name = "FIXTURE")]
        committer: PathBuf,
        #[command(flatten)]
        generate: GenerateArgs,
    },
}

/// The options every generated fixture takes.
#[derive(Args)]
struct GenerateArgs {
    /// Where to write the fixture. Every resource nonce derives from its file
    /// stem (and `--salt`), so fixtures with different names never share a
    /// nullifier.
    out_path: PathBuf,
    /// Set this run's fixtures apart from every other run's on the same
    /// deployment: resource nonces derive from it too, so nothing the run
    /// settles was spent before.
    #[arg(long, value_name = "SALT")]
    salt: Option<String>,
    #[command(flatten)]
    prover: ProverArgs,
    /// Also write the final transaction's error variants to DIR:
    /// wrong_root, no_aggregation, garbage_proof, corrupt_seal, zero_action,
    /// foreign_kind_table and witness_delta.
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
    /// The test forwarder in the given mode, expecting an empty output.
    TestForwarder(u8),
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

/// The verifier selector of a transaction's seal-encoded aggregation proof.
fn seal_selector(tx: &Transaction) -> Result<[u8; 4]> {
    let seal: Seal = Seal::try_from_slice(&require_aggregation(tx)?.proof)
        .context("decode Seal from aggregation proof bytes")?;
    Ok(seal.selector)
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

fn block_time_forwarder_external_payload_blob(output_mismatch: bool) -> ExpirableBlob {
    let program_id = block_time_forwarder::ID.to_bytes();

    // Use -1 so expected_time < current_time for any reasonable cluster clock.
    // The forwarder will return RESULT_LT (0x00).
    let input = (-1_i64).to_le_bytes().to_vec();

    // If output_mismatch is true, set expected_output to RESULT_GT which is WRONG.
    // The forwarder will return RESULT_LT, but we expect RESULT_GT, causing ForwarderCallOutputMismatch.
    let expected_output = if output_mismatch {
        vec![RESULT_GT]
    } else {
        vec![RESULT_LT]
    };

    encode_external_call(&SolanaExternalCall {
        program_id,
        instruction_data: input,
        expected_output,
        output_mode: OutputMode::ReturnData,
        num_accounts: 2,
    })
}

/// A call to the test forwarder in `mode`, expecting an empty output.
fn test_forwarder_payload_blob(mode: u8) -> ExpirableBlob {
    encode_external_call(&SolanaExternalCall {
        program_id: test_forwarder::ID.to_bytes(),
        instruction_data: vec![mode],
        expected_output: vec![],
        output_mode: OutputMode::ReturnData,
        num_accounts: 1,
    })
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

/// A fixture's name, which its resource nonces derive from: its file stem
/// (`batch_groth16` for `tests/fixtures/batch_groth16.json`), followed by
/// `/<salt>` when the run is salted.
fn fixture_name(path: &Path, salt: Option<&str>) -> Result<String> {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| {
            anyhow!(
                "{} has no UTF-8 file stem to name the fixture",
                path.display()
            )
        })?;
    Ok(match salt {
        Some(salt) => format!("{stem}/{salt}"),
        None => stem.to_string(),
    })
}

/// The name an existing fixture was generated under, which the resources it
/// created derive from.
fn read_fixture_name(path: &Path) -> Result<String> {
    #[derive(Deserialize)]
    struct Named {
        name: String,
    }
    let named: Named = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("{} carries no fixture name", path.display()))?;
    Ok(named.name)
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
        logic_ref: PASSTHROUGH_LOGIC_VK,
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
    let passthrough_vk = PASSTHROUGH_LOGIC_VK;

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
        PASSTHROUGH_LOGIC_PK,
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
        } => block_time_forwarder_external_payload_blob(*output_mismatch),
        ForwarderMode::TestForwarder(mode) => test_forwarder_payload_blob(*mode),
    };
    consumed_app_data.external_payload.push(external_blob);
    if let ForwarderMode::BlockTimeForwarder {
        multi_external_call: true,
        ..
    } = forwarder_mode
    {
        consumed_app_data
            .external_payload
            .push(block_time_forwarder_external_payload_blob(false));
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
    let passthrough_vk = PASSTHROUGH_LOGIC_VK;
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
                PASSTHROUGH_LOGIC_PK,
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
    name: &str,
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
            name: name.to_owned(),
            format: FIXTURE_FORMAT,
            aggregation_strategy: "batch",
            aggregation_proof_type: proof_type,
            selector: selector.to_owned(),
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
        // The transaction claiming a kind table that is neither the empty
        // one nor any deployment's: the adapter refuses its commitment before
        // it verifies the proof.
        let mut foreign_kind_table = tx.clone();
        require_aggregation_mut(&mut foreign_kind_table)?
            .instance
            .kind_table_commitment = Digest::from_bytes([0x4b; 32]);
        write_variant("foreign_kind_table.json", &foreign_kind_table)?;
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

    let selector = format!(
        "0x{}",
        hex::encode(seal_selector(tx).context("extract selector from proof")?)
    );
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
/// the resulting `Fixture` JSON under `name`.
fn finalize_and_write_fixture(
    tx: &mut Transaction,
    out_path: &Path,
    name: String,
) -> Result<Fixture> {
    // The client crate's settlement transaction: dev-mode (Fake) receipts
    // become mock seals for the localnet mock verifier, real Groth16
    // receipts arm's seal. The fixture is labeled by the seal's selector
    // (aggregation_proof_type, selector).
    let proof_type = timed_phase("encode_seal", || {
        *tx = settlement_transaction(tx.clone()).context("encode the aggregation seal")?;
        Ok(if seal_selector(tx)? == MOCK_SELECTOR {
            "mock"
        } else {
            "groth16"
        })
    })?;

    let fields = timed_phase("derive_fixture_fields", || derive_fixture_fields(tx))?;
    let fixture = Fixture {
        name,
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: proof_type,
        selector: fields.selector,
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
    let passthrough_vk = PASSTHROUGH_LOGIC_VK;

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

/// The historical-root consumer: spends the resource the committer fixture
/// `committer_name` created, the last of `leaves` (the tree settled before
/// the consumer), through its Merkle path. The root that path reconstructs is
/// a real historical root, never the initial one, so settlement must find
/// its marker.
async fn generate_historical_root_consumer_transaction(
    prover: &Prover,
    committer_name: &str,
    leaves: &[Digest],
) -> Result<Transaction> {
    let (_, committed_resource, committer_nf_key) =
        build_historical_root_committer_witness(committer_name)?;
    let committed_cm = committed_resource.commitment();
    let index = leaves.len().checked_sub(1).ok_or_else(|| {
        anyhow!("the consumer needs the leaves settled before it, the committer's last")
    })?;
    if leaves[index] != committed_cm {
        bail!(
            "the last leaf must be the committer's created commitment {}, found {}",
            hex::encode(committed_cm.as_bytes()),
            hex::encode(leaves[index].as_bytes())
        );
    }
    let (merkle_path, root) = checked_pa_merkle_path(leaves, index)?;
    eprintln!(
        "verified: the committed resource is leaf {index} of {}; root {}",
        leaves.len(),
        hex::encode(root.as_bytes())
    );
    let witness =
        build_historical_root_consumer_witness(committed_resource, committer_nf_key, merkle_path)?;
    prove_single_action_transaction(prover, witness, AppData::default()).await
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
        Command::Generate(shape) => generate_fixture(shape).await,
    }
}

async fn generate_fixture(shape: ShapeCommand) -> Result<()> {
    let (ShapeCommand::Batch { generate, .. }
    | ShapeCommand::OutputMismatch { generate }
    | ShapeCommand::ForwarderFail { generate }
    | ShapeCommand::ForwarderSilent { generate }
    | ShapeCommand::ForwarderEmptyOutput { generate }
    | ShapeCommand::ConsumeOnly { generate }
    | ShapeCommand::TransferShape { generate }
    | ShapeCommand::HistoricalRootCommitter { generate }
    | ShapeCommand::HistoricalRootConsumer { generate, .. }) = &shape;
    let GenerateArgs {
        out_path,
        salt,
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
            "mode: output-mismatch (intentionally wrong expected_output for ForwarderCallOutputMismatch test)"
        ),
        ShapeCommand::ConsumeOnly { .. } => {
            eprintln!("mode: consume-only (one zero-quantity consumed resource, nothing created)")
        }
        ShapeCommand::TransferShape { .. } => eprintln!(
            "mode: transfer-shape ({TRANSFER_SHAPE_ACTIONS} actions, event-emitted payloads, no external calls)"
        ),
        ShapeCommand::HistoricalRootCommitter { .. } => {
            eprintln!("mode: historical-root committer (creates a non-ephemeral resource)")
        }
        ShapeCommand::HistoricalRootConsumer { committer, .. } => eprintln!(
            "mode: historical-root consumer (spends committer {}'s resource)",
            committer.display()
        ),
        ShapeCommand::Batch { .. }
        | ShapeCommand::ForwarderFail { .. }
        | ShapeCommand::ForwarderSilent { .. }
        | ShapeCommand::ForwarderEmptyOutput { .. } => {}
    }
    if let Some(dir) = &error_variants {
        eprintln!("error variants output dir: {}", dir.display());
    }

    eprintln!("phase: generate_test_transaction");
    let gen_start = Instant::now();
    let name = fixture_name(out_path, salt.as_deref())?;
    let name = name.as_str();
    let single_action = |mode| generate_test_transaction_with_external_payload(&prover, mode, name);
    let mut tx = match &shape {
        ShapeCommand::Batch {
            multi_external_call,
            ..
        } => {
            single_action(ForwarderMode::BlockTimeForwarder {
                output_mismatch: false,
                multi_external_call: *multi_external_call,
            })
            .await?
        }
        ShapeCommand::OutputMismatch { .. } => {
            single_action(ForwarderMode::BlockTimeForwarder {
                output_mismatch: true,
                multi_external_call: false,
            })
            .await?
        }
        ShapeCommand::ForwarderFail { .. } => {
            single_action(ForwarderMode::TestForwarder(MODE_FAIL)).await?
        }
        ShapeCommand::ForwarderSilent { .. } => {
            single_action(ForwarderMode::TestForwarder(MODE_SILENT)).await?
        }
        ShapeCommand::ForwarderEmptyOutput { .. } => {
            single_action(ForwarderMode::TestForwarder(MODE_EMPTY)).await?
        }
        ShapeCommand::ConsumeOnly { .. } => {
            generate_consume_only_transaction(&prover, name).await?
        }
        ShapeCommand::TransferShape { .. } => {
            generate_transfer_shape_transaction(&prover, name).await?
        }
        ShapeCommand::HistoricalRootCommitter { .. } => {
            let (witness, _, _) = build_historical_root_committer_witness(name)?;
            prove_single_action_transaction(&prover, witness, AppData::default()).await?
        }
        ShapeCommand::HistoricalRootConsumer { committer, .. } => {
            generate_historical_root_consumer_transaction(
                &prover,
                &read_fixture_name(committer)?,
                &created_commitments(&load_fixture_tx(committer)?)?,
            )
            .await?
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

    let fixture = finalize_and_write_fixture(&mut tx, out_path, name.to_string())?;

    if matches!(shape, ShapeCommand::TransferShape { .. }) {
        check_transfer_shape_wire_size(&fixture.tx_b64)?;
    }

    if let Some(dir) = error_variants.as_deref() {
        timed_phase("write_error_variants", || {
            generate_error_variant_fixtures(
                &tx,
                &fixture.name,
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

    /// A salted run names its fixtures apart from the unsalted set and from
    /// every other salt, so their resources never share a nullifier.
    #[test]
    fn salt_sets_the_fixture_name_apart() {
        let path = Path::new("tests/fixtures/batch_groth16.json");
        assert_eq!(fixture_name(path, None).unwrap(), "batch_groth16");
        assert_eq!(
            fixture_name(path, Some("run1")).unwrap(),
            "batch_groth16/run1"
        );
        assert_ne!(
            fixture_nullifier(&fixture_name(path, Some("run1")).unwrap(), 0),
            fixture_nullifier(&fixture_name(path, Some("run2")).unwrap(), 0),
            "two salted runs must not share a nullifier"
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
        assert!(parse(&[
            "historical-root-consumer",
            "--committer",
            "a.json",
            "out.json"
        ])
        .is_ok());
        for rejected in [
            &["historical-root-consumer", "out.json"][..],
            &["forwarder-fail", "--multi-external-call", "out.json"],
            &["transfer-shape", "--multi-external-call", "out.json"],
            &["batch", "--committer", "a.json", "out.json"],
            &["output-mismatch", "--multi-external-call", "out.json"],
            &[
                "historical-root-consumer",
                "--committer",
                "a.json",
                "--committer",
                "b.json",
                "out.json",
            ],
            &[
                "historical-root-committer",
                "--multi-external-call",
                "out.json",
            ],
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
            fixture_nullifier("batch_a", 0),
            fixture_nullifier("batch_b", 0),
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
        let passthrough_vk = PASSTHROUGH_LOGIC_VK;
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
            PASSTHROUGH_LOGIC_PK,
            &consumed_instance,
            LocalProofType::Succinct,
        )
        .unwrap();
        let (crp, crj) = arm::proving_system::prove(
            PASSTHROUGH_LOGIC_PK,
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
