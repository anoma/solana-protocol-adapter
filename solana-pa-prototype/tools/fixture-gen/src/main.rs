use anchor_lang::prelude::{AnchorDeserialize as BorshDeserialize, AnchorSerialize, Pubkey};
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
    recipient_seed_label: &'static str,
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

enum Command {
    Generate(GenerateArgs),
    Dump {
        input: PathBuf,
    },
    HistoricalRoot {
        batch_groth16_path: PathBuf,
        committer_out: PathBuf,
        consumer_out: PathBuf,
        prover_choice: Option<ProverChoice>,
        mock: bool,
    },
}

struct GenerateArgs {
    debug_assumptions: bool,
    shape: GenerateShape,
    error_variants_dir: Option<PathBuf>,
    out_path: PathBuf,
    prover_choice: Option<ProverChoice>,
    mock: bool,
    /// The kind table the fixture is proven against (`--kind-table`).
    kind_table: PathBuf,
}

/// What kind of transaction the default `Generate` command builds.
enum GenerateShape {
    /// One action with an external forwarder call bound into its app data.
    SingleAction {
        forwarder_mode: ForwarderMode,
        multi_external_call: bool,
    },
    /// Three single-unit actions with event-emitted payload blobs and no
    /// external calls — the captured mainnet transfer's shape (OOM
    /// regression); see `generate_transfer_shape_transaction`.
    TransferShape,
    /// An AnomaPay SPL token wrap proven with the real transfer logic, under
    /// the forwarder nonce `nonce`; see `generate_anomapay_wrap_transaction`.
    AnomaPayWrap { nonce: u64 },
    /// An AnomaPay SPL token unwrap spending the wrap's resource through a
    /// Merkle path over the fixtures settled before it (`--settled`, in
    /// settlement order, the wrap last); see
    /// `generate_anomapay_unwrap_transaction`.
    AnomaPayUnwrap { settled: Vec<PathBuf> },
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
    seal.try_to_vec().context("serialize mock Seal")
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

/// Flip one bit of the aggregation instance's first created commitment.
/// The transaction still decodes and settles structurally, but the journal
/// digest the PA recomputes from the mutated instance no longer matches the
/// proof, so verification must fail.
fn mutate_created_commitment_keep_structure(tx: &mut Transaction) -> Result<()> {
    let instance = &mut require_aggregation_mut(tx)?.instance;
    let action = instance
        .actions
        .get_mut(0)
        .ok_or_else(|| anyhow!("aggregation instance has no actions"))?;
    let created = action
        .created_publics
        .get_mut(0)
        .ok_or_else(|| anyhow!("aggregation instance has no created resources"))?;

    let mut bytes = <[u8; 32]>::from(created.resource_commitment);
    bytes[0] ^= 1;
    created.resource_commitment = Digest::from_bytes(bytes);
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
/// recipient ATA, escrow PDA, token program).
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
    let compliance_unit = prove_compliance(prover, &action.compliance_witness)
        .await
        .context("prove compliance")?;
    let logic_verifiers = prove_logic_pair(
        prover,
        TOKEN_TRANSFER_ELF,
        &TOKEN_TRANSFER_ID,
        action.consumed_logic.witness,
        action.created_logic.witness,
    )
    .await?;
    let proven = arm::action::new(compliance_unit, logic_verifiers)
        .map_err(|e| anyhow!("build AnomaPay action: {e:?}"))?;
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
        .action(auth, &discovery_pk(), anomapay_compliance_params())
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

/// The created commitments of the fixtures at `paths`, in order: the leaves
/// their settlements append.
fn tree_leaves_of(paths: &[PathBuf]) -> Result<Vec<Digest>> {
    let mut leaves = Vec::new();
    for path in paths {
        let tx = load_fixture_tx(path)?;
        let commitments = created_commitments(&tx)?;
        if commitments.is_empty() {
            bail!(
                "{} settles no commitment, it is not a tree leaf",
                path.display()
            );
        }
        leaves.extend(commitments);
    }
    Ok(leaves)
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

    let unwrap = action::unwrap(
        actors.label.clone(),
        wrap.created,
        owner.keys.clone(),
        actors.recipient,
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
        recipient_seed_label: RECIPIENT_SEED_LABEL,
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

    let compliance_unit = prove_compliance(prover, compliance_witness)
        .await
        .context("prove compliance")?;

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

    let logic_verifiers = prove_logic_pair(
        prover,
        PASSTHROUGH_LOGIC_GUEST_ELF,
        &passthrough_vk,
        consumed_instance,
        created_instance,
    )
    .await?;
    arm::action::new(compliance_unit, logic_verifiers).map_err(|e| anyhow!("build action: {e:?}"))
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
    let mut verifiers = Vec::with_capacity(2);
    for (witness, label) in [(consumed, "consumed"), (created, "created")] {
        let (proof, instance) = prove_logic(prover, proving_key, verifying_key, witness)
            .await
            .with_context(|| format!("prove the {label} resource's logic"))?;
        verifiers.push(LogicVerifier {
            proof,
            instance,
            verifying_key: *verifying_key,
        });
    }
    Ok(verifiers)
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
    let mut actions = Vec::with_capacity(TRANSFER_SHAPE_ACTIONS);
    let mut rcvs = Vec::with_capacity(TRANSFER_SHAPE_ACTIONS);
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

        actions.push(prove_action(prover, &witness, AppData::default(), created_app_data).await?);
        rcvs.push(witness.rcv);
    }

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
    multi_external_call: bool,
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
        ForwarderMode::BlockTimeForwarder { output_mismatch } => {
            block_time_forwarder_external_payload_blob(*output_mismatch)?
        }
        ForwarderMode::TestForwarderFail => test_forwarder_fail_payload_blob()?,
        ForwarderMode::TestForwarderSilent => test_forwarder_silent_payload_blob()?,
        ForwarderMode::TestForwarderRelay => test_forwarder_relay_payload_blob()?,
    };
    consumed_app_data.external_payload.push(external_blob);
    if multi_external_call && matches!(forwarder_mode, ForwarderMode::BlockTimeForwarder { .. }) {
        consumed_app_data
            .external_payload
            .push(block_time_forwarder_external_payload_blob(false)?);
    }

    prove_single_action_transaction(prover, compliance_witness, consumed_app_data).await
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
        aggregation.proof = seal.try_to_vec().context("serialize corrupted Seal")?;
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
    mutate_created_commitment_keep_structure(&mut tx_tampered)?;
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
        build_historical_root_committer_witness(fixture_name(committer_out)?)?;
    let mut committer_tx =
        prove_single_action_transaction(&prover, committer_witness, AppData::default())
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
    arm::transaction::verify_aggregation(&committer_tx, JournalEncoding::Risc0Serde)
        .map_err(|e| anyhow!("verify committer aggregated proof: {e:?}"))?;

    finalize_and_write_fixture(&mut committer_tx, committer_out, None)
        .context("write committer fixture")?;

    // The committer settles right after batch_groth16 (leaf 0), as leaf 1.
    let committed_cm = committed_resource.commitment();
    let (merkle_path, expected_root) =
        checked_pa_merkle_path(&[batch_groth16_leaf, committed_cm], 1)?;

    eprintln!("phase: generate historical-root consumer transaction");
    let consume_start = Instant::now();
    let consumer_witness =
        build_historical_root_consumer_witness(committed_resource, committer_nf_key, merkle_path)
            .context("build consumer witness")?;
    let mut consumer_tx =
        prove_single_action_transaction(&prover, consumer_witness, AppData::default())
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
    arm::transaction::verify_aggregation(&consumer_tx, JournalEncoding::Risc0Serde)
        .map_err(|e| anyhow!("verify consumer aggregated proof: {e:?}"))?;

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

fn print_usage() {
    eprintln!(
        "Usage:\n  fixture-gen [OPTIONS] [OUT_PATH]         Generate a fixture (default)\n  fixture-gen dump <IN>                    Print transaction structure\n  fixture-gen historical-root <batch_groth16.json> <committer_out.json> <consumer_out.json> [--prover local|queue] [--mock]\n                                            Generate the historical-root committer/consumer fixture pair\n\nGenerate options:\n  --debug-assumptions      Print claim digests for composition debugging\n  --output-mismatch        Wrong expected_output for ExternalCallOutputMismatch test\n  --forwarder-fail         Test-forwarder with failing instruction\n  --forwarder-silent       Test-forwarder with no return data\n  --forwarder-relay        Test-forwarder relaying an unwrap to the SPL forwarder\n                           (the forwarder must reject a caller other than the adapter)\n  --spl-token-wrap         AnomaPay wrap proven with the transfer logic: the user's\n                           ed25519-authorized escrow deposit creates the owner's resource\n  --spl-token-unwrap       AnomaPay unwrap: the owner spends the wrapped resource, releasing\n                           the escrow to the recipient; needs --settled\n  --wrap-nonce N           (wrap) the forwarder nonce the user signs (default 1)\n  --kind-table PATH        Prove against this kind table instead of the committed\n                           empty one (kind_table.json); the PA must store its commitment\n  --settled FIXTURE        (unwrap) a fixture settled before the unwrap, in settlement\n                           order, the wrap last; repeat per fixture\n  --multi-external-call    Append a second block-time-forwarder external call blob\n  --transfer-shape         Three single-unit actions with event-emitted payload blobs\n                           and no external calls (the captured mainnet transfer's shape);\n                           excludes the forwarder flags\n  --error-variants DIR     Write wrong_root/no_aggregation/garbage_proof/corrupt_seal/\n                           zero_action/witness_delta variants\n  --mock                   Dev-mode executor instead of proving (seconds, no GPU or\n                           podman proving step); emits a mock seal (selector 0xffffffff)\n                           only the localnet mock verifier accepts\n  --prover <local|queue>   Select the prover backend (default: queue if QUEUE_BASE_URL\n                           is set, local otherwise). local runs risc0's CPU prover\n                           in-process; queue dispatches to the AnomaPay workers queue\n                           via QUEUE_BASE_URL/QUEUE_AUTH_TOKEN.\n\nNotes:\n  - At most one of --output-mismatch, --forwarder-fail, --forwarder-silent,\n    --forwarder-relay, --spl-token-wrap, --spl-token-unwrap.\n  - With --prover queue (or no --prover and QUEUE_BASE_URL set), QUEUE_BASE_URL and\n    QUEUE_AUTH_TOKEN must be set; proofs are dispatched to the workers queue.\n  - With --prover local (or no --prover and QUEUE_BASE_URL unset), proofs run on the\n    local CPU risc0 prover; the Groth16 aggregation step needs a container runtime\n    (podman/docker) for the STARK -> Groth16 wrapper.\n  - --error-variants writes to DIR from the final aggregated tx.\n  - Every resource nonce derives from the output file's stem, so fixtures with\n    different names never share a nullifier.\n"
    );
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
            "dump" => {
                if raw_args.len() != 2 {
                    return Err(anyhow!("Usage: fixture-gen dump <input.json>"));
                }
                let input = PathBuf::from(raw_args.remove(1));
                return Ok(Command::Dump { input });
            }
            "historical-root" => {
                let args = raw_args.into_iter().skip(1);
                return parse_historical_root_args(args);
            }
            _ => {}
        }
    }

    let mut args = raw_args.into_iter();
    let mut debug_assumptions = false;
    let mut forwarder_mode: Option<ForwarderMode> = None;
    let mut multi_external_call = false;
    let mut transfer_shape = false;
    let mut error_variants_dir: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut prover_choice: Option<ProverChoice> = None;
    let mut mock = false;
    let mut anomapay: Option<GenerateShape> = None;
    let mut settled: Vec<PathBuf> = Vec::new();
    let mut wrap_nonce: Option<u64> = None;
    let mut kind_table: Option<PathBuf> = None;

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
            "--output-mismatch" | "--forwarder-fail" | "--forwarder-silent"
            | "--forwarder-relay" | "--spl-token-wrap" | "--spl-token-unwrap" => {
                if forwarder_mode.is_some() || anomapay.is_some() {
                    return Err(anyhow!(
                        "at most one of --output-mismatch, --forwarder-fail, --forwarder-silent, \
                         --forwarder-relay, --spl-token-wrap, --spl-token-unwrap may be set"
                    ));
                }
                match flag {
                    "--output-mismatch" => {
                        forwarder_mode = Some(ForwarderMode::BlockTimeForwarder {
                            output_mismatch: true,
                        })
                    }
                    "--forwarder-fail" => forwarder_mode = Some(ForwarderMode::TestForwarderFail),
                    "--forwarder-silent" => {
                        forwarder_mode = Some(ForwarderMode::TestForwarderSilent)
                    }
                    "--forwarder-relay" => forwarder_mode = Some(ForwarderMode::TestForwarderRelay),
                    "--spl-token-wrap" => {
                        anomapay = Some(GenerateShape::AnomaPayWrap {
                            nonce: ANOMAPAY_WRAP_NONCE,
                        })
                    }
                    "--spl-token-unwrap" => {
                        anomapay = Some(GenerateShape::AnomaPayUnwrap { settled: vec![] })
                    }
                    _ => unreachable!(),
                }
            }
            "--wrap-nonce" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--wrap-nonce requires a value"))?;
                wrap_nonce = Some(
                    value
                        .parse::<u64>()
                        .with_context(|| format!("invalid --wrap-nonce value: {value}"))?,
                );
            }
            "--kind-table" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--kind-table requires a path"))?;
                kind_table = Some(PathBuf::from(value));
            }
            "--settled" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--settled requires a fixture path"))?;
                settled.push(PathBuf::from(value));
            }
            "--multi-external-call" => {
                multi_external_call = true;
            }
            "--transfer-shape" => {
                transfer_shape = true;
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

    if let Some(GenerateShape::AnomaPayUnwrap { settled: leaves }) = &mut anomapay {
        if settled.is_empty() {
            return Err(anyhow!(
                "--spl-token-unwrap needs the fixtures settled before it, in order, \
                 as --settled arguments, the wrap fixture last"
            ));
        }
        *leaves = std::mem::take(&mut settled);
    }
    if !settled.is_empty() {
        return Err(anyhow!("--settled only applies to --spl-token-unwrap"));
    }
    if let Some(value) = wrap_nonce {
        match &mut anomapay {
            Some(GenerateShape::AnomaPayWrap { nonce }) => *nonce = value,
            _ => return Err(anyhow!("--wrap-nonce only applies to --spl-token-wrap")),
        }
    }

    let shape = if transfer_shape {
        if forwarder_mode.is_some() || anomapay.is_some() || multi_external_call {
            return Err(anyhow!(
                "--transfer-shape has no external calls; it cannot combine with \
                 --output-mismatch/--forwarder-fail/--forwarder-silent/--spl-token-wrap/\
                 --spl-token-unwrap/--multi-external-call"
            ));
        }
        GenerateShape::TransferShape
    } else if let Some(shape) = anomapay {
        if multi_external_call {
            return Err(anyhow!(
                "--multi-external-call applies to the block-time forwarder, not to \
                 --spl-token-wrap/--spl-token-unwrap"
            ));
        }
        shape
    } else {
        GenerateShape::SingleAction {
            forwarder_mode: forwarder_mode.unwrap_or(ForwarderMode::BlockTimeForwarder {
                output_mismatch: false,
            }),
            multi_external_call,
        }
    };

    Ok(Command::Generate(GenerateArgs {
        debug_assumptions,
        shape,
        error_variants_dir,
        out_path,
        prover_choice,
        mock,
        kind_table: kind_table.unwrap_or_else(|| PathBuf::from(KIND_TABLE_PATH)),
    }))
}

/// Enter mock mode (no-op unless `mock`): proofs run through the local
/// dev-mode executor (guests execute, nothing is proven), and
/// `finalize_and_write_fixture` turns the resulting Fake aggregation receipt
/// into a mock seal. RISC0_DEV_MODE is a runtime env var read by risc0 at
/// proving time; setting it here keeps the flag self-contained instead of
/// depending on ambient environment state.
fn apply_mock_mode(
    mock: bool,
    prover_choice: Option<ProverChoice>,
) -> Result<Option<ProverChoice>> {
    if !mock {
        return Ok(prover_choice);
    }
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
    let command = parse_args()?;
    // Every proving and verification path checks witness kind tables against
    // the globally loaded table, so load one before any command runs: the
    // generated fixture's `--kind-table`, the committed table otherwise.
    let kind_table = match &command {
        Command::Generate(args) => args.kind_table.clone(),
        _ => PathBuf::from(KIND_TABLE_PATH),
    };
    init_kind_table_from_file(&kind_table)
        .map_err(|e| anyhow!("load kind table {}: {e:?}", kind_table.display()))?;
    eprintln!(
        "kind table: {} (commitment {})",
        kind_table.display(),
        hex::encode(
            kind_table_hash()
                .ok_or_else(|| anyhow!("kind table not loaded"))?
                .as_bytes()
        )
    );

    let GenerateArgs {
        debug_assumptions,
        shape,
        error_variants_dir,
        out_path,
        prover_choice,
        mock,
        kind_table: _,
    } = match command {
        Command::Dump { input } => return dump_fixture(&input),
        Command::HistoricalRoot {
            batch_groth16_path,
            committer_out,
            consumer_out,
            prover_choice,
            mock,
        } => {
            let prover_choice = apply_mock_mode(mock, prover_choice)?;
            return generate_historical_root_fixtures(
                &batch_groth16_path,
                &committer_out,
                &consumer_out,
                prover_choice,
            )
            .await;
        }
        Command::Generate(args) => args,
    };

    let total_start = Instant::now();

    let prover_choice = apply_mock_mode(mock, prover_choice)?;
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
    match &shape {
        GenerateShape::SingleAction {
            forwarder_mode:
                ForwarderMode::BlockTimeForwarder {
                    output_mismatch: true,
                },
            ..
        } => eprintln!(
            "mode: output-mismatch (intentionally wrong expected_output for ExternalCallOutputMismatch test)"
        ),
        GenerateShape::SingleAction {
            multi_external_call: true,
            ..
        } => eprintln!("mode: multi-external-call (two external payload blobs)"),
        GenerateShape::SingleAction { .. } => {}
        GenerateShape::TransferShape => eprintln!(
            "mode: transfer-shape ({TRANSFER_SHAPE_ACTIONS} actions, event-emitted payloads, no external calls)"
        ),
        GenerateShape::AnomaPayWrap { nonce } => eprintln!(
            "mode: AnomaPay wrap (transfer logic {}, forwarder nonce {nonce})",
            TOKEN_TRANSFER_ID
        ),
        GenerateShape::AnomaPayUnwrap { settled } => eprintln!(
            "mode: AnomaPay unwrap (transfer logic {}, {} settled fixtures)",
            TOKEN_TRANSFER_ID,
            settled.len()
        ),
    }
    if let Some(dir) = &error_variants_dir {
        eprintln!("error variants output dir: {}", dir.display());
    }

    eprintln!("phase: generate_test_transaction");
    let gen_start = Instant::now();
    let is_transfer_shape = matches!(shape, GenerateShape::TransferShape);
    let name = fixture_name(&out_path)?;
    let (mut tx, spl_forwarder) = match shape {
        GenerateShape::SingleAction {
            forwarder_mode,
            multi_external_call,
        } => (
            generate_test_transaction_with_external_payload(
                &prover,
                forwarder_mode,
                name,
                multi_external_call,
            )
            .await?,
            None,
        ),
        GenerateShape::TransferShape => (
            generate_transfer_shape_transaction(&prover, name).await?,
            None,
        ),
        GenerateShape::AnomaPayWrap { nonce } => {
            let (tx, metadata) = generate_anomapay_wrap_transaction(&prover, name, nonce).await?;
            (tx, Some(metadata))
        }
        GenerateShape::AnomaPayUnwrap { settled } => {
            let wrap_path = settled
                .last()
                .ok_or_else(|| anyhow!("--spl-token-unwrap needs the wrap fixture as --settled"))?;
            let leaves = tree_leaves_of(&settled)?;
            let (tx, metadata) =
                generate_anomapay_unwrap_transaction(&prover, fixture_name(wrap_path)?, &leaves)
                    .await?;
            (tx, Some(metadata))
        }
    };
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
        arm::transaction::verify_aggregation(&tx, JournalEncoding::Risc0Serde)
            .map_err(|e| anyhow!("verify aggregated proof: {e:?}"))
    })?;

    let fixture = finalize_and_write_fixture(&mut tx, &out_path, spl_forwarder)?;

    if is_transfer_shape {
        check_transfer_shape_wire_size(&fixture.tx_b64)?;
    }

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
        let (tx, _) = generate_anomapay_unwrap_transaction(&Prover::Local, wrap_name, &leaves)
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
