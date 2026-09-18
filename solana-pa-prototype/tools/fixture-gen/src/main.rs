use anchor_lang::prelude::{AnchorDeserialize as BorshDeserialize, AnchorSerialize, Pubkey};
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
use arm::proving_system::{encode_seal, ProofType as LocalProofType};
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
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use verifier_router::Seal;

use passthrough_logic_methods::{PASSTHROUGH_LOGIC_GUEST_ELF, PASSTHROUGH_LOGIC_GUEST_ID};

/// The kind table every fixture commits to: the committed empty table. The
/// compliance circuit hashes the witness's table into the instance, and the
/// PA pins that commitment at initialization, so fixtures and deployment
/// tooling must agree on this file.
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
        arm::transaction::aggregate(&mut tx, LocalProofType::Groth16)
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
use solana_pa::external_calls::decode_external_call;
use solana_pa::external_calls::encode_external_call;
use solana_pa::state::PAStateAccount;
use solana_pa::types::{OutputMode, SolanaExternalCall};
use spl_token_forwarder::state::{UnwrapInput, WrapInput};
use spl_token_forwarder::{OP_UNWRAP, OP_WRAP, RESULT_SUCCESS as SPL_RESULT_SUCCESS};
use test_forwarder::{MODE_FAIL, MODE_SILENT};
use transfer_library::action::{self, ComplianceParams, Owner, TransferAction, Wrap, WrapAuth};
use transfer_library::{TOKEN_TRANSFER_ELF, TOKEN_TRANSFER_ID};
use transfer_witness::call_type::{UNWRAP_SEGMENT_NUM_ACCOUNTS, WRAP_SEGMENT_NUM_ACCOUNTS};
use transfer_witness::{LabelInfo, TokenTransferWitness, AUTH_SIGNATURE_DOMAIN};

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

/// Fixture fields that are not derived from the transaction: what kind of
/// forwarder call it carries and, for the SPL forwarder, the replay data.
#[derive(Default, Serialize)]
struct FixtureLabels {
    #[serde(skip_serializing_if = "Option::is_none")]
    forwarder_type: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spl_token_wrap: Option<SplTokenWrapMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spl_token_unwrap: Option<SplTokenUnwrapMetadata>,
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
    labels: FixtureLabels,
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
    RefreshFields {
        path: PathBuf,
    },
}

struct GenerateArgs {
    debug_assumptions: bool,
    shape: GenerateShape,
    nonce_seed: Option<u8>,
    error_variants_dir: Option<PathBuf>,
    out_path: PathBuf,
    prover_choice: Option<ProverChoice>,
    mock: bool,
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
    /// An AnomaPay SPL token wrap proven with the real transfer logic; see
    /// `generate_anomapay_wrap_transaction`.
    AnomaPayWrap,
    /// An AnomaPay SPL token unwrap spending the wrap's resource through a
    /// Merkle path over the fixtures settled before it (`tree_leaves`, in
    /// suite order, the wrap last); see `generate_anomapay_unwrap_transaction`.
    AnomaPayUnwrap { tree_leaves: Vec<PathBuf> },
}

/// Which SPL token forwarder call `--spl-token-wrap`/`--spl-token-unwrap` ask for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnomaPayCall {
    Wrap,
    Unwrap,
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
        proof: groth_16_verifier::Proof {
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

fn decode_base58_32(s: &str) -> Result<[u8; 32]> {
    let bytes = bs58::decode(s)
        .into_vec()
        .with_context(|| format!("invalid base58: {s}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| anyhow!("expected 32 bytes, got {}", v.len()))
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

/// Nonce byte of the ephemeral resource the wrap consumes (20; the replay
/// fixture passes 21 through `--nonce-seed`). See the reservation map at
/// `HISTORICAL_ROOT_NONCE_BYTE`. The unwrap recomputes the wrapped resource
/// from the wrap's default nonce byte, so the wrap fixture must use it.
const ANOMAPAY_WRAP_NONCE_BYTE: u8 = 20;
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

/// A secp256k1 scalar from a label: the first eight bytes of its hash, which
/// is enough entropy for test keys and always a valid scalar.
fn scalar_from_label(label: &str) -> Scalar {
    let hash = label_hash(label);
    Scalar::from(u64::from_le_bytes(hash[..8].try_into().unwrap()))
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
                auth_pk: AuthorityVerifyingKey::from_signing_key(&auth_sk),
                encryption_pk: generate_public_key(encryption_sk.inner()),
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

    fn user_pubkey(&self) -> [u8; 32] {
        self.user.verifying_key().to_bytes()
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

/// The seeded wrap of 100 tokens whose consumed resource carries
/// `nonce_byte`, and the parties behind it.
fn seeded_wrap(nonce_byte: u8) -> Result<(AnomaPayActors, SeededOwner, Wrap)> {
    let actors = AnomaPayActors::seeded();
    let owner = SeededOwner::seeded();
    let wrap = action::wrap(
        actors.label.clone(),
        ANOMAPAY_AMOUNT,
        nonce_from_byte(nonce_byte),
        &owner.keys,
        label_hash(WRAPPED_RAND_SEED_LABEL),
    )
    .map_err(|e| anyhow!("build the wrap's resources: {e:?}"))?;
    Ok((actors, owner, wrap))
}

/// Prove one AnomaPay action through the selected prover: the compliance
/// proof plus the two transfer logic proofs, in canonical tag order
/// (consumed, then created).
async fn prove_anomapay_action(prover: &Prover, action: TransferAction) -> Result<Action> {
    let compliance_unit = prove_compliance(prover, &action.compliance_witness)
        .await
        .context("prove compliance")?;
    let (consumed_proof, consumed_journal) = prove_logic(
        prover,
        TOKEN_TRANSFER_ELF,
        &TOKEN_TRANSFER_ID,
        action.consumed_logic.witness,
    )
    .await
    .context("prove the consumed resource's transfer logic")?;
    let (created_proof, created_journal) = prove_logic(
        prover,
        TOKEN_TRANSFER_ELF,
        &TOKEN_TRANSFER_ID,
        action.created_logic.witness,
    )
    .await
    .context("prove the created resource's transfer logic")?;
    let verifier = |proof, instance| LogicVerifier {
        proof,
        instance,
        verifying_key: *TOKEN_TRANSFER_ID,
    };
    arm::action::new(
        compliance_unit,
        vec![
            verifier(consumed_proof, consumed_journal),
            verifier(created_proof, created_journal),
        ],
    )
    .map_err(|e| anyhow!("build AnomaPay action: {e:?}"))
}

/// The forwarder operand an ephemeral witness commits: its one external
/// payload blob must decode to a call on the forwarder with the segment
/// size and op code the forwarder expects. Checked before proving, on the
/// host-side witness, since the guest commits exactly this payload.
fn forwarder_operand(
    witness: &TokenTransferWitness,
    action_tree_root: &Digest,
    num_accounts: u8,
    op: u8,
) -> Result<Vec<u8>> {
    let payload = witness
        .ephemeral_resource_check(action_tree_root.as_bytes())
        .map_err(|e| anyhow!("the transfer logic rejects the witness: {e:?}"))?;
    if payload.len() != 1 {
        bail!(
            "the witness commits {} external calls, expected one",
            payload.len()
        );
    }
    let call = decode_external_call(&payload[0])
        .map_err(|e| anyhow!("decode the circuit's forwarder call: {e:?}"))?;
    if call.program_id != spl_token_forwarder::ID.to_bytes() {
        bail!("the circuit's forwarder call targets another program");
    }
    if call.expected_output != vec![SPL_RESULT_SUCCESS] {
        bail!("the circuit's forwarder call expects an output the forwarder never returns");
    }
    if call.num_accounts != num_accounts {
        bail!(
            "the call commits {} accounts, the forwarder's segment has {num_accounts}",
            call.num_accounts
        );
    }
    match call.instruction_data.split_first() {
        Some((first, operand)) if *first == op => Ok(operand.to_vec()),
        Some((first, _)) => bail!("the call carries op {first}, expected {op}"),
        None => bail!("empty forwarder instruction data"),
    }
}

/// The wrap input the forwarder parses from the witness's call.
fn wrap_input_of(witness: &TokenTransferWitness, action_tree_root: &Digest) -> Result<WrapInput> {
    let operand = forwarder_operand(
        witness,
        action_tree_root,
        WRAP_SEGMENT_NUM_ACCOUNTS,
        OP_WRAP,
    )?;
    WrapInput::try_from_bytes(&operand)
        .map_err(|e| anyhow!("the forwarder rejects the circuit's wrap input: {e:?}"))
}

/// The unwrap input the forwarder parses from the witness's call.
fn unwrap_input_of(
    witness: &TokenTransferWitness,
    action_tree_root: &Digest,
) -> Result<UnwrapInput> {
    let operand = forwarder_operand(
        witness,
        action_tree_root,
        UNWRAP_SEGMENT_NUM_ACCOUNTS,
        OP_UNWRAP,
    )?;
    UnwrapInput::try_from_bytes(&operand)
        .map_err(|e| anyhow!("the forwarder rejects the circuit's unwrap input: {e:?}"))
}

/// The AnomaPay wrap the fixture settles: the seeded user wraps 100 tokens
/// of the seeded mint, consuming an ephemeral resource whose transfer logic
/// commits the forwarder call, and creating the owner's shielded resource
/// whose logic emits the encrypted payloads. The user signs the message the
/// forwarder recomputes from the proof-bound input.
async fn generate_anomapay_wrap_transaction(
    prover: &Prover,
    nonce_seed: Option<u8>,
) -> Result<(Transaction, FixtureLabels)> {
    let (actors, owner, wrap) = seeded_wrap(nonce_seed.unwrap_or(ANOMAPAY_WRAP_NONCE_BYTE))?;
    let auth = WrapAuth {
        user: actors.user_pubkey(),
        nonce: ANOMAPAY_WRAP_NONCE,
        deadline: ANOMAPAY_WRAP_DEADLINE,
        ed25519_ix_index: ANOMAPAY_ED25519_IX_INDEX,
    };
    let action = wrap.action(
        &auth,
        &owner.keys,
        &discovery_pk(),
        anomapay_compliance_params(),
    );

    // The forwarder parses the call the circuit commits, and recomputes the
    // message the user signed from it: both must agree with the resource.
    let input = wrap_input_of(&action.consumed_logic.witness, &action.action_tree_root)?;
    let signed_message = wrap.signed_message(&auth);
    if input.to_message(&spl_token_forwarder::ID).signed_message() != signed_message.as_bytes() {
        bail!("the forwarder recomputes a different signed message than the resource produced");
    }
    let signature = actors.user.sign(signed_message.as_bytes()).to_bytes();

    let rcv = action.compliance_witness.rcv.clone();
    let action = prove_anomapay_action(prover, action).await?;
    let tx = assemble_transaction(vec![action], std::slice::from_ref(&rcv))?;

    let labels = FixtureLabels {
        forwarder_type: Some("spl_token_wrap"),
        spl_token_wrap: Some(SplTokenWrapMetadata {
            user_seed_label: USER_SEED_LABEL,
            mint_seed_label: MINT_SEED_LABEL,
            amount: input.amount,
            nonce: input.nonce,
            signed_message_b64: BASE64.encode(signed_message),
            signature_b64: BASE64.encode(signature),
            logic_ref_b64: token_transfer_logic_ref_b64(),
        }),
        ..Default::default()
    };
    Ok((tx, labels))
}

/// The created commitments of the fixtures at `paths`, in order: the leaves
/// the suite's settlements append.
fn tree_leaves_of(paths: &[PathBuf]) -> Result<Vec<Digest>> {
    let mut leaves = Vec::new();
    for path in paths {
        let (_, tx) = load_fixture_tx(path)?;
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
    let mut state =
        PAStateAccount::running(0, Pubkey::default(), Pubkey::default(), [0; 4], [0; 32]);
    for leaf in leaves {
        solana_pa::merkle::append_to_tree(&mut state, *leaf)
            .map_err(|e| anyhow!("replay append_to_tree: {e:?}"))?;
    }
    Ok(Digest::from_bytes(state.root))
}

/// The Merkle path of leaf `index` in the adapter's tree over `leaves`: the
/// sibling at each of `required_depth_for_leaves(n)` levels, empty slots
/// padded with the zero subtrees.
fn pa_merkle_path(leaves: &[Digest], index: usize) -> MerklePath {
    let depth = solana_pa::merkle::required_depth_for_leaves(leaves.len() as u64);
    let mut nodes = leaves.to_vec();
    let mut position = index;
    let mut path = Vec::with_capacity(depth);
    for level in 0..depth {
        let zero = solana_pa::merkle::ZEROS[level];
        path.push((
            nodes.get(position ^ 1).copied().unwrap_or(zero),
            position & 1 == 1,
        ));
        nodes = nodes
            .chunks(2)
            .map(|pair| {
                let right = pair.get(1).copied().unwrap_or(zero);
                solana_pa::merkle::hash_two(&pair[0], &right)
            })
            .collect();
        position >>= 1;
    }
    MerklePath::from_path(&path)
}

/// The Merkle path of leaf `index` and the root it reconstructs, checked
/// against the adapter's own replay of the tree: a path that disagrees with
/// the program's growth rule fails here, not at settlement.
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

/// The AnomaPay unwrap the fixture settles: the owner spends the wrapped
/// resource, the last of `leaves` (the tree the suite has built by then),
/// into an ephemeral resource whose transfer logic commits the unwrap call
/// releasing the 100 tokens to the seeded recipient.
async fn generate_anomapay_unwrap_transaction(
    prover: &Prover,
    leaves: &[Digest],
) -> Result<(Transaction, FixtureLabels)> {
    let (actors, owner, wrap) = seeded_wrap(ANOMAPAY_WRAP_NONCE_BYTE)?;
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
        &owner.keys.nf_key,
        actors.recipient,
    )
    .map_err(|e| anyhow!("build the unwrap's resources: {e:?}"))?;
    let auth_sig = owner
        .auth_sk
        .sign(AUTH_SIGNATURE_DOMAIN, unwrap.action_tree_root.as_bytes());
    let action = unwrap.action(&owner.keys, auth_sig, path, anomapay_compliance_params());
    let input = unwrap_input_of(&action.created_logic.witness, &action.action_tree_root)?;

    let rcv = action.compliance_witness.rcv.clone();
    let action = prove_anomapay_action(prover, action).await?;
    let tx = assemble_transaction(vec![action], std::slice::from_ref(&rcv))?;

    let labels = FixtureLabels {
        forwarder_type: Some("spl_token_unwrap"),
        spl_token_unwrap: Some(SplTokenUnwrapMetadata {
            mint_seed_label: MINT_SEED_LABEL,
            amount: input.amount,
            recipient_seed_label: RECIPIENT_SEED_LABEL,
            logic_ref_b64: token_transfer_logic_ref_b64(),
        }),
        ..Default::default()
    };
    Ok((tx, labels))
}

/// The action tree root of a one-consumed, one-created action: nullifier
/// then commitment, the order the aggregation guest enforces.
fn single_action_tree_root(consumed_nf: Digest, created_cm: Digest) -> Result<Digest> {
    ActionTree::new(vec![consumed_nf, created_cm])
        .root()
        .map_err(|e| anyhow!("compute action tree root: {e:?}"))
}

/// A nonce whose first half is `byte`: distinct per fixture, so their
/// nullifiers never collide in one suite.
fn nonce_from_byte(byte: u8) -> [u8; 32] {
    let mut nonce = [0u8; 32];
    nonce[..16].fill(byte);
    nonce
}

/// An ephemeral resource under `nf_key`.
fn ephemeral_resource(
    logic_ref: Digest,
    label_ref: Digest,
    value_ref: Digest,
    quantity: u128,
    nonce: [u8; 32],
    nf_key: &NullifierKey,
) -> Resource {
    Resource {
        logic_ref,
        label_ref,
        value_ref,
        quantity,
        is_ephemeral: true,
        nonce,
        nk_commitment: nf_key.commit(),
        ..Default::default()
    }
}

/// A deterministic passthrough-logic ephemeral resource with the given nonce
/// byte, plus its nullifier under the default nullifier key.
fn deterministic_ephemeral_resource(nonce_byte: u8) -> Result<(Resource, NullifierKey, Digest)> {
    let nf_key = NullifierKey::default();
    let consumed_resource = ephemeral_resource(
        Digest::new(PASSTHROUGH_LOGIC_GUEST_ID),
        Digest::default(),
        Digest::default(),
        1,
        nonce_from_byte(nonce_byte),
        &nf_key,
    );
    let consumed_nf = consumed_resource
        .nullifier(&nf_key)
        .map_err(|e| anyhow!("compute consumed nullifier: {e:?}"))?;
    Ok((consumed_resource, nf_key, consumed_nf))
}

/// Build the compliance witness for a single-consumed / single-created
/// action. Built through `from_parts` with a fixed, caller-chosen `rcv`
/// rather than arm's randomized `from_resources*` constructors, which draw
/// a fresh `rcv` and would make fixture generation nondeterministic.
/// Carries the globally loaded kind table, so every instance commits to
/// the table hash the PA pins at initialization.
fn single_action_compliance_witness_with_rcv(
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

fn single_action_compliance_witness(
    consumed: Resource,
    cm_merkle_path: MerklePath,
    nf_key: NullifierKey,
    created: Resource,
) -> ComplianceWitness {
    single_action_compliance_witness_with_rcv(
        consumed,
        cm_merkle_path,
        nf_key,
        created,
        Scalar::ONE,
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
        proof: consumed_proof,
        instance: consumed_journal,
        verifying_key: passthrough_vk,
    };
    let created_logic = LogicVerifier {
        proof: created_proof,
        instance: created_journal,
        verifying_key: passthrough_vk,
    };

    // Logic verifiers in canonical tag order: consumed nullifiers first,
    // then created commitments — the order the aggregation guest enforces.
    arm::action::new(compliance_unit, vec![consumed_logic, created_logic])
        .map_err(|e| anyhow!("build action: {e:?}"))
}

/// Wrap proven actions into a balanced, delta-proved `Transaction`. The delta
/// witness composes every action's `rcv`.
fn assemble_transaction(actions: Vec<Action>, rcvs: &[Vec<u8>]) -> Result<Transaction> {
    let delta_witness = arm::delta_proof::from_bytes_vec(rcvs)
        .map_err(|e| anyhow!("build delta witness: {e:?}"))?;

    let tx = Transaction::create(actions, Delta::Witness(delta_witness));
    let balanced_tx = arm::transaction::generate_delta_proof(tx)
        .map_err(|e| anyhow!("generate delta proof: {e:?}"))?;
    let kind_table_commitment =
        *kind_table_hash().ok_or_else(|| anyhow!("kind table not loaded"))?;
    arm::transaction::verify(&balanced_tx, kind_table_commitment)
        .map_err(|e| anyhow!("verify tx: {e:?}"))?;

    Ok(balanced_tx)
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
/// actions (nonce bytes `base_seed..base_seed+2`, distinct rcvs so the
/// delta points differ, as with production's random rcvs), each created
/// resource carrying event-emitted payload blobs. No external calls — the
/// real transfer had none.
async fn generate_transfer_shape_transaction(
    prover: &Prover,
    nonce_seed: Option<u8>,
) -> Result<Transaction> {
    let base_seed = nonce_seed.unwrap_or(TRANSFER_SHAPE_NONCE_BYTE);

    let mut actions = Vec::with_capacity(TRANSFER_SHAPE_ACTIONS);
    let mut rcvs = Vec::with_capacity(TRANSFER_SHAPE_ACTIONS);
    for i in 0..TRANSFER_SHAPE_ACTIONS {
        let nonce_byte = base_seed
            .checked_add(i as u8)
            .ok_or_else(|| anyhow!("nonce seed {base_seed} + {i} overflows a byte"))?;
        let (consumed_resource, nf_key, consumed_nf) =
            deterministic_ephemeral_resource(nonce_byte)?;

        let mut created_resource = consumed_resource;
        created_resource.nonce = Resource::derive_nonce_from_nullifiers(0, &[consumed_nf])
            .map_err(|e| anyhow!("derive created nonce: {e:?}"))?;

        // Distinct rcv per action: identical rcvs (with identical kinds and
        // quantities) would collapse the actions' delta points onto one
        // point, which is not the shape production transactions have.
        let rcv = Scalar::from((i + 1) as u64);
        let witness = single_action_compliance_witness_with_rcv(
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
    nonce_seed: Option<u8>,
    multi_external_call: bool,
) -> Result<Transaction> {
    // Stable nonce so the fixture is deterministic. Each fixture variant uses
    // a different nonce so the variants have different nullifiers; otherwise
    // running several fixtures in one test suite hits DuplicateNullifier.
    // See HISTORICAL_ROOT_NONCE_BYTE for the reservation map.
    let nonce_byte: u8 = nonce_seed.unwrap_or(match &forwarder_mode {
        ForwarderMode::BlockTimeForwarder {
            output_mismatch: true,
        } => 2,
        _ => 0,
    });
    let (consumed_resource, nf_key, consumed_nf) = deterministic_ephemeral_resource(nonce_byte)?;

    let mut created_resource = consumed_resource;
    // The compliance circuit requires created nonces to be derived from the
    // action's consumed nullifiers (index 0: first created resource).
    created_resource.nonce = Resource::derive_nonce_from_nullifiers(0, &[consumed_nf])
        .map_err(|e| anyhow!("derive created nonce: {e:?}"))?;
    // The consumed resource is ephemeral, so it needs no inclusion proof.
    let compliance_witness = single_action_compliance_witness(
        consumed_resource,
        MerklePath::empty(),
        nf_key,
        created_resource,
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
            labels: FixtureLabels::default(),
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

/// Fields every fixture derives from a seal-encoded transaction: the
/// serialized transaction and a serialized tampered clone (base64), its
/// consumed nullifiers and historical roots (base64), and the seal's
/// selector. Shared by the generate, import, and mockify paths so every
/// fixture follows the same on-disk convention.
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

fn import_backend_result_fixture(
    input: &Path,
    output: &Path,
    root_account_dir: Option<&Path>,
    program_id: [u8; 32],
) -> Result<()> {
    let raw = fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let mut tx: Transaction = serde_json::from_slice(&raw)
        .with_context(|| format!("decode backend Transaction JSON {}", input.display()))?;

    arm::transaction::verify_aggregation(&tx)
        .map_err(|e| anyhow!("verify imported backend aggregation proof: {e:?}"))?;

    let roots = historical_roots(&tx).context("extract imported historical roots")?;

    let aggregation = require_aggregation_mut(&mut tx)?;
    aggregation.proof =
        encode_seal(&aggregation.proof).map_err(|e| anyhow!("encode imported seal: {e:?}"))?;

    let fields = derive_fixture_fields(&tx).context("derive imported fixture fields")?;
    let fixture = Fixture {
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: "groth16",
        selector: fields.selector,
        labels: FixtureLabels {
            forwarder_type: Some("anomapay_transfer"),
            ..Default::default()
        },
        tx_b64: fields.tx_b64,
        tx_tampered_b64: fields.tx_tampered_b64,
        consumed_nullifiers_b64: fields.consumed_nullifiers_b64,
        created_commitments_b64: fields.created_commitments_b64,
        historical_roots_b64: fields.historical_roots_b64,
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
    labels: FixtureLabels,
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
        labels,
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

/// Nonce byte reserved for the historical-root committer/consumer pair.
/// Existing fixtures use 0 (default), 2 (output-mismatch), 3-7
/// (`--nonce-seed`, see v2/v3/multi-call/forwarder-fail/forwarder-silent),
/// 9-11 (transfer shape), and 20-21 (AnomaPay wrap and its replay; the
/// unwrap's nullifiers derive from the wrapped resource), so 8 avoids a
/// `DuplicateNullifier` collision.
const HISTORICAL_ROOT_NONCE_BYTE: u8 = 8;

/// Base nonce byte for the transfer-shape fixture's three actions (9-11;
/// see `HISTORICAL_ROOT_NONCE_BYTE` for the full reservation map).
const TRANSFER_SHAPE_NONCE_BYTE: u8 = 9;

/// The one commitment an existing fixture settles: the leaf the suite
/// inserts at index 0 when that fixture is `batch_groth16.json`.
fn read_sole_created_commitment(path: &Path) -> Result<Digest> {
    let leaves = tree_leaves_of(std::slice::from_ref(&path.to_path_buf()))?;
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
/// build the consumer transaction afterward.
fn build_historical_root_committer_witness() -> Result<(ComplianceWitness, Resource, NullifierKey)>
{
    let (consumed_resource, nf_key, consumed_nf) =
        deterministic_ephemeral_resource(HISTORICAL_ROOT_NONCE_BYTE)?;

    let mut created_resource = consumed_resource;
    created_resource.nonce = Resource::derive_nonce_from_nullifiers(0, &[consumed_nf])
        .map_err(|e| anyhow!("derive committer created nonce: {e:?}"))?;
    created_resource.is_ephemeral = false;

    let compliance_witness = single_action_compliance_witness(
        consumed_resource,
        MerklePath::empty(),
        nf_key.clone(),
        created_resource,
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
    let mut created_resource = Resource {
        logic_ref: passthrough_vk,
        nk_commitment: output_nf_key.commit(),
        quantity: 1,
        is_ephemeral: true,
        ..Default::default()
    };
    created_resource.nonce = Resource::derive_nonce_from_nullifiers(0, &[consumed_nf])
        .map_err(|e| anyhow!("derive consumer created nonce: {e:?}"))?;

    Ok(single_action_compliance_witness(
        committed_resource,
        merkle_path,
        committer_nf_key,
        created_resource,
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
        build_historical_root_committer_witness()?;
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
    arm::transaction::verify_aggregation(&committer_tx)
        .map_err(|e| anyhow!("verify committer aggregated proof: {e:?}"))?;

    finalize_and_write_fixture(
        &mut committer_tx,
        committer_out,
        FixtureLabels {
            forwarder_type: Some("historical_root_committer"),
            ..Default::default()
        },
    )
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
    arm::transaction::verify_aggregation(&consumer_tx)
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

    finalize_and_write_fixture(
        &mut consumer_tx,
        consumer_out,
        FixtureLabels {
            forwarder_type: Some("historical_root_consumer"),
            ..Default::default()
        },
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

/// Read a fixture JSON and bincode-decode its transaction. Returns the raw
/// JSON map alongside so callers can rewrite fields in place.
fn load_fixture_tx(
    path: &Path,
) -> Result<(serde_json::Map<String, serde_json::Value>, Transaction)> {
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).context("parsing fixture JSON")?;
    let tx_b64 = map
        .get("tx_b64")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing tx_b64 field in {}", path.display()))?;
    let tx_bytes = BASE64.decode(tx_b64).context("decoding tx_b64")?;
    let tx: Transaction =
        bincode::deserialize(&tx_bytes).context("deserializing Transaction from bincode")?;
    Ok((map, tx))
}

fn strip_calls_from_fixture(input: &Path, output: &Path) -> Result<()> {
    let (mut fixture, mut tx) = load_fixture_tx(input)?;

    let mut stripped = 0usize;
    let instance = &mut require_aggregation_mut(&mut tx)?.instance;
    for action in &mut instance.actions {
        let resources = action
            .consumed_publics
            .iter_mut()
            .map(|c| (&c.resource_nullifier, &mut c.app_data))
            .chain(
                action
                    .created_publics
                    .iter_mut()
                    .map(|c| (&c.resource_commitment, &mut c.app_data)),
            );
        for (tag, app_data) in resources {
            let count = app_data.external_payload.len();
            if count > 0 {
                eprintln!(
                    "  Stripping {} external_payload blob(s) from resource tag {:?}",
                    count,
                    &tag.as_bytes()[..4]
                );
                app_data.external_payload.clear();
                stripped += count;
            }
        }
    }
    eprintln!("Stripped {} external call(s) total", stripped);

    let modified_bytes = bincode::serialize(&tx).context("re-serializing Transaction")?;
    fixture.insert("tx_b64".into(), BASE64.encode(&modified_bytes).into());
    fixture.remove("forwarder_type");

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
    let (mut fixture, mut tx) = load_fixture_tx(input)?;

    let claim = mock_claim_digest(&tx)?;
    require_aggregation_mut(&mut tx)?.proof = mock_seal_bytes(claim)?;

    let fields = derive_fixture_fields(&tx)?;

    // The mock twin must change nothing but the seal: the recomputed
    // derived fields must match the input fixture exactly.
    let existing_nullifiers: Vec<String> =
        serde_json::from_value(fixture["consumed_nullifiers_b64"].clone())
            .context("parsing consumed_nullifiers_b64")?;
    if fields.consumed_nullifiers_b64 != existing_nullifiers {
        bail!("recomputed nullifiers differ from the input fixture's — refusing to write");
    }
    let existing_roots: Vec<String> = fixture
        .get("historical_roots_b64")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("parsing historical_roots_b64")?
        .unwrap_or_default();
    if fields.historical_roots_b64 != existing_roots {
        bail!("recomputed historical roots differ from the input fixture's — refusing to write");
    }

    fixture.insert("tx_b64".into(), fields.tx_b64.into());
    fixture.insert("tx_tampered_b64".into(), fields.tx_tampered_b64.into());
    fixture.insert(
        "created_commitments_b64".into(),
        fields.created_commitments_b64.into(),
    );
    fixture.insert("selector".into(), fields.selector.into());
    fixture.insert("aggregation_proof_type".into(), "mock".into());

    fs::write(output, serde_json::to_string_pretty(&fixture)?)
        .with_context(|| format!("writing {}", output.display()))?;
    eprintln!("wrote mock fixture: {}", output.display());
    Ok(())
}

/// Re-derive every transaction-derived fixture field from the fixture's own
/// `tx_b64` and rewrite the JSON in place. The transaction (and therefore
/// the proof) is untouched, so this needs no proving — it exists to migrate
/// committed fixtures when the derived-field set changes.
fn refresh_fixture_fields(path: &Path) -> Result<()> {
    let (mut fixture, tx) = load_fixture_tx(path)?;

    let fields = derive_fixture_fields(&tx)?;
    if fields.tx_b64 != fixture["tx_b64"].as_str().unwrap_or_default() {
        bail!("re-serialized transaction differs from tx_b64 — refusing to rewrite");
    }

    fixture.insert("tx_tampered_b64".into(), fields.tx_tampered_b64.into());
    fixture.insert("selector".into(), fields.selector.into());
    fixture.insert(
        "consumed_nullifiers_b64".into(),
        fields.consumed_nullifiers_b64.into(),
    );
    fixture.insert(
        "created_commitments_b64".into(),
        fields.created_commitments_b64.into(),
    );
    if fields.historical_roots_b64.is_empty() {
        fixture.remove("historical_roots_b64");
    } else {
        fixture.insert(
            "historical_roots_b64".into(),
            fields.historical_roots_b64.into(),
        );
    }

    fs::write(path, serde_json::to_string_pretty(&fixture)?)
        .with_context(|| format!("writing {}", path.display()))?;
    eprintln!("refreshed fixture fields: {}", path.display());
    Ok(())
}

fn dump_fixture(input: &Path) -> Result<()> {
    let (_, tx) = load_fixture_tx(input)?;

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
        "Usage:\n  fixture-gen [OPTIONS] [OUT_PATH]         Generate a fixture (default)\n  fixture-gen import-backend-result --program-id PROGRAM_ID_B58 [--root-account-dir DIR] <IN_JSON> <OUT_JSON>\n  fixture-gen strip-calls <IN> <OUT>       Remove external calls from a fixture\n  fixture-gen dump <IN>                    Print transaction structure\n  fixture-gen mockify <IN> <OUT>           Convert an existing fixture into its mock twin\n  fixture-gen refresh-fields <FIXTURE>     Re-derive fixture fields from tx_b64 in place (no proving)\n  fixture-gen historical-root <batch_groth16.json> <committer_out.json> <consumer_out.json> [--prover local|queue] [--mock]\n                                            Generate the historical-root committer/consumer fixture pair\n\nGenerate options:\n  --debug-assumptions      Print claim digests for composition debugging\n  --output-mismatch        Wrong expected_output for ExternalCallOutputMismatch test\n  --forwarder-fail         Test-forwarder with failing instruction\n  --forwarder-silent       Test-forwarder with no return data\n  --spl-token-wrap         AnomaPay wrap proven with the transfer logic: the user's\n                           ed25519-authorized escrow deposit creates the owner's resource\n  --spl-token-unwrap       AnomaPay unwrap: the owner spends the wrapped resource, releasing\n                           the escrow to the recipient; needs --tree-leaf\n  --tree-leaf FIXTURE      (unwrap) a fixture the suite settles before the unwrap, in\n                           settlement order, the wrap last; repeat per fixture\n  --nonce-seed N           Override deterministic nonce byte for nullifier derivation\n  --multi-external-call    Append a second block-time-forwarder external call blob\n  --transfer-shape         Three single-unit actions with event-emitted payload blobs\n                           and no external calls (the captured mainnet transfer's shape);\n                           excludes the forwarder flags. Nonce bytes seed..seed+2 (default 9)\n  --error-variants DIR     Write wrong_root/no_aggregation/garbage_proof/zero_action/witness_delta variants\n  --mock                   Dev-mode executor instead of proving (seconds, no GPU or\n                           podman proving step); emits a mock seal (selector 0xffffffff)\n                           only the localnet mock verifier accepts\n  --prover <local|queue>   Select the prover backend (default: queue if QUEUE_BASE_URL\n                           is set, local otherwise). local runs risc0's CPU prover\n                           in-process; queue dispatches to the AnomaPay workers queue\n                           via QUEUE_BASE_URL/QUEUE_AUTH_TOKEN.\n\nNotes:\n  - At most one of --output-mismatch, --forwarder-fail, --forwarder-silent,\n    --spl-token-wrap, --spl-token-unwrap.\n  - import-backend-result converts backend Transaction JSON into the on-chain TxData bincode fixture.\n  - With --prover queue (or no --prover and QUEUE_BASE_URL set), QUEUE_BASE_URL and\n    QUEUE_AUTH_TOKEN must be set; proofs are dispatched to the workers queue.\n  - With --prover local (or no --prover and QUEUE_BASE_URL unset), proofs run on the\n    local CPU risc0 prover; the Groth16 aggregation step needs a container runtime\n    (podman/docker) for the STARK -> Groth16 wrapper.\n  - --error-variants writes to DIR from the final aggregated tx.\n  - mockify replaces only the aggregation seal of an existing fixture; use it for\n    imported fixtures whose proving inputs are not in this repo.\n"
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
            "refresh-fields" => {
                if raw_args.len() != 2 {
                    return Err(anyhow!("Usage: fixture-gen refresh-fields <fixture.json>"));
                }
                let path = PathBuf::from(raw_args.remove(1));
                return Ok(Command::RefreshFields { path });
            }
            _ => {}
        }
    }

    let mut args = raw_args.into_iter();
    let mut debug_assumptions = false;
    let mut forwarder_mode: Option<ForwarderMode> = None;
    let mut nonce_seed: Option<u8> = None;
    let mut multi_external_call = false;
    let mut transfer_shape = false;
    let mut error_variants_dir: Option<PathBuf> = None;
    let mut out_path: Option<PathBuf> = None;
    let mut prover_choice: Option<ProverChoice> = None;
    let mut mock = false;
    let mut anomapay: Option<AnomaPayCall> = None;
    let mut tree_leaves: Vec<PathBuf> = Vec::new();

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
            | "--spl-token-wrap" | "--spl-token-unwrap" => {
                if forwarder_mode.is_some() || anomapay.is_some() {
                    return Err(anyhow!(
                        "at most one of --output-mismatch, --forwarder-fail, --forwarder-silent, \
                         --spl-token-wrap, --spl-token-unwrap may be set"
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
                    "--spl-token-wrap" => anomapay = Some(AnomaPayCall::Wrap),
                    "--spl-token-unwrap" => anomapay = Some(AnomaPayCall::Unwrap),
                    _ => unreachable!(),
                }
            }
            "--tree-leaf" => {
                let value = eq_value
                    .map(|s| s.to_string())
                    .or_else(|| args.next())
                    .ok_or_else(|| anyhow!("--tree-leaf requires a fixture path"))?;
                tree_leaves.push(PathBuf::from(value));
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

    if !tree_leaves.is_empty() && anomapay != Some(AnomaPayCall::Unwrap) {
        return Err(anyhow!("--tree-leaf only applies to --spl-token-unwrap"));
    }
    if anomapay == Some(AnomaPayCall::Unwrap) {
        if nonce_seed.is_some() {
            return Err(anyhow!(
                "--spl-token-unwrap derives its nonces from the wrapped resource; \
                 --nonce-seed does not apply"
            ));
        }
        if tree_leaves.is_empty() {
            return Err(anyhow!(
                "--spl-token-unwrap needs the fixtures the suite settles before it, in order, \
                 as --tree-leaf arguments, the wrap fixture last"
            ));
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
    } else if let Some(call) = anomapay {
        if multi_external_call {
            return Err(anyhow!(
                "--multi-external-call applies to the block-time forwarder, not to \
                 --spl-token-wrap/--spl-token-unwrap"
            ));
        }
        match call {
            AnomaPayCall::Wrap => GenerateShape::AnomaPayWrap,
            AnomaPayCall::Unwrap => GenerateShape::AnomaPayUnwrap { tree_leaves },
        }
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
        nonce_seed,
        error_variants_dir,
        out_path,
        prover_choice,
        mock,
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
    // Every proving and verification path checks witness kind tables against
    // the globally loaded table, so load the committed table unconditionally.
    init_kind_table_from_file(Path::new(KIND_TABLE_PATH))
        .map_err(|e| anyhow!("load kind table {KIND_TABLE_PATH}: {e:?}"))?;

    let GenerateArgs {
        debug_assumptions,
        shape,
        nonce_seed,
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
            let prover_choice = apply_mock_mode(mock, prover_choice)?;
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
        Command::RefreshFields { path } => {
            return refresh_fixture_fields(&path);
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
        GenerateShape::AnomaPayWrap => {
            eprintln!("mode: AnomaPay wrap (transfer logic {})", *TOKEN_TRANSFER_ID)
        }
        GenerateShape::AnomaPayUnwrap { tree_leaves } => eprintln!(
            "mode: AnomaPay unwrap (transfer logic {}, {} tree-leaf fixtures)",
            *TOKEN_TRANSFER_ID,
            tree_leaves.len()
        ),
    }
    if let Some(seed) = nonce_seed {
        eprintln!("mode: nonce-seed override ({seed})");
    }
    if let Some(dir) = &error_variants_dir {
        eprintln!("error variants output dir: {}", dir.display());
    }

    eprintln!("phase: generate_test_transaction");
    let gen_start = Instant::now();
    let is_transfer_shape = matches!(shape, GenerateShape::TransferShape);
    let (mut tx, labels) = match shape {
        GenerateShape::SingleAction {
            forwarder_mode,
            multi_external_call,
        } => (
            generate_test_transaction_with_external_payload(
                &prover,
                forwarder_mode,
                nonce_seed,
                multi_external_call,
            )
            .await?,
            FixtureLabels::default(),
        ),
        GenerateShape::TransferShape => (
            generate_transfer_shape_transaction(&prover, nonce_seed).await?,
            FixtureLabels::default(),
        ),
        GenerateShape::AnomaPayWrap => {
            generate_anomapay_wrap_transaction(&prover, nonce_seed).await?
        }
        GenerateShape::AnomaPayUnwrap { tree_leaves } => {
            let leaves = tree_leaves_of(&tree_leaves)?;
            generate_anomapay_unwrap_transaction(&prover, &leaves).await?
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
        arm::transaction::verify_aggregation(&tx)
            .map_err(|e| anyhow!("verify aggregated proof: {e:?}"))
    })?;

    let fixture = finalize_and_write_fixture(&mut tx, &out_path, labels)?;

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

    /// The wrap creates the resource the unwrap recomputes, both proven
    /// under the transfer logic. Dev-mode proving through the real guest.
    #[tokio::test(flavor = "multi_thread")]
    async fn anomapay_wrap_creates_the_resource_the_unwrap_consumes() {
        init_test_kind_table();
        let (tx, _) = generate_anomapay_wrap_transaction(&Prover::Local, None)
            .await
            .unwrap();
        let wrapped = seeded_wrap(ANOMAPAY_WRAP_NONCE_BYTE).unwrap().2.created;
        let action = &tx.actions.as_ref().unwrap()[0];
        assert_eq!(action.logic_verifier_inputs[1].tag, wrapped.commitment());
        for input in &action.logic_verifier_inputs {
            assert_eq!(input.verifying_key, *TOKEN_TRANSFER_ID);
        }
    }

    /// An unwrap over a synthetic tree spends the wrapped resource at the
    /// last leaf.
    #[tokio::test(flavor = "multi_thread")]
    async fn anomapay_unwrap_consumes_the_wrap_leaf() {
        init_test_kind_table();
        let (_, owner, wrap) = seeded_wrap(ANOMAPAY_WRAP_NONCE_BYTE).unwrap();
        let wrapped = wrap.created;
        let mut leaves = synthetic_leaves(9);
        leaves.push(wrapped.commitment());

        let (tx, _) = generate_anomapay_unwrap_transaction(&Prover::Local, &leaves)
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
            *TOKEN_TRANSFER_ID
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
        init_test_kind_table();
        let (consumed, nf_key, consumed_nf) = deterministic_ephemeral_resource(nonce_byte).unwrap();
        let passthrough_vk = Digest::new(PASSTHROUGH_LOGIC_GUEST_ID);

        let mut created = consumed;
        created.nonce = Resource::derive_nonce_from_nullifiers(0, &[consumed_nf]).unwrap();
        let created_cm = created.commitment();

        let witness =
            single_action_compliance_witness(consumed, MerklePath::empty(), nf_key, created);
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
        let tx = build_valid_tx_with_delta_proof(100);
        arm::transaction::verify(&tx, *kind_table_hash().unwrap()).unwrap();
    }

    /// Swapping the nullifier and commitment tags in the delta message
    /// must invalidate the delta proof signature.
    #[test]
    fn swapped_tags_invalidate_delta_proof() {
        let tx = build_valid_tx_with_delta_proof(101);
        arm::transaction::verify(&tx, *kind_table_hash().unwrap()).unwrap();

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

        let result = arm::transaction::verify(&swapped, *kind_table_hash().unwrap());
        assert!(result.is_err(), "swapped nf/cm must invalidate delta proof");
    }

    /// Mutating a single delta coordinate must invalidate the delta proof.
    #[test]
    fn mutated_delta_x_invalidates_proof() {
        let mut tx = build_valid_tx_with_delta_proof(102);
        arm::transaction::verify(&tx, *kind_table_hash().unwrap()).unwrap();

        // Flip a word in delta_x
        let actions = tx.actions.as_mut().unwrap();
        mutate_compliance_instance(&mut actions[0].compliance_unit, |inst| {
            inst.delta_x[0] ^= 0xFFFFFFFF;
        })
        .unwrap();

        let result = arm::transaction::verify(&tx, *kind_table_hash().unwrap());
        assert!(
            result.is_err(),
            "mutated delta_x must invalidate delta proof"
        );
    }

    /// Mutating a nullifier must invalidate the delta proof (changes the message hash).
    #[test]
    fn mutated_nullifier_invalidates_delta_proof() {
        let mut tx = build_valid_tx_with_delta_proof(103);
        arm::transaction::verify(&tx, *kind_table_hash().unwrap()).unwrap();

        // Corrupt the nullifier
        let actions = tx.actions.as_mut().unwrap();
        mutate_compliance_instance(&mut actions[0].compliance_unit, |inst| {
            inst.consumed_publics[0].resource_nullifier = Digest::from_bytes([0xFF; 32]);
        })
        .unwrap();

        let result = arm::transaction::verify(&tx, *kind_table_hash().unwrap());
        assert!(
            result.is_err(),
            "mutated nullifier must invalidate delta proof"
        );
    }
}
