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
use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha256Digest, Sha256};
use ed25519_dalek::{Signer, SigningKey};
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
use spl_token_forwarder::{OP_WRAP, OP_UNWRAP, RESULT_SUCCESS as SPL_RESULT_SUCCESS};
use test_forwarder::{MODE_FAIL, MODE_SILENT, MODE_WRITE_ACCOUNT};

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SplTokenWrapMetadata {
    user_secret_key_b64: String,
    user_pubkey_b64: String,
    mint_seed_b64: String,
    token_mint_b58: String,
    amount: u64,
    nonce: u64,
    deadline: i64,
    action_tree_root_b64: String,
    signature_b64: String,
    logic_ref_b64: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SplTokenUnwrapMetadata {
    mint_seed_b64: String,
    token_mint_b58: String,
    amount: u64,
    recipient_seed_b64: String,
    recipient_b58: String,
    logic_ref_b64: String,
}

#[derive(Serialize)]
struct Fixture {
    format: &'static str,
    aggregation_strategy: &'static str,
    aggregation_proof_type: &'static str,
    /// Groth16 verifier selector extracted from the proof's verifier_parameters.
    /// Format: "0x" + 4-byte hex (e.g., "0x73c457ba").
    selector: String,
    forwarder_type: &'static str,
    tx_b64: String,
    tx_tampered_b64: String,
    consumed_nullifiers_b64: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spl_token_wrap: Option<SplTokenWrapMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spl_token_unwrap: Option<SplTokenUnwrapMetadata>,
}

const FIXTURE_FORMAT: &str = "arm-risc0:Transaction(bincode)";

#[derive(Deserialize)]
struct FixtureInput {
    format: String,
    selector: String,
    tx_b64: String,
    tx_tampered_b64: String,
    #[serde(default)]
    consumed_nullifiers_b64: Vec<String>,
}

enum CliCommand {
    Generate(CliArgs),
    Validate {
        fixture_path: PathBuf,
        program_id: [u8; 32],
    },
    StripCalls {
        input: PathBuf,
        output: PathBuf,
    },
    Dump {
        input: PathBuf,
    },
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
    SplTokenWrap { output_mismatch: bool },
    SplTokenUnwrap { output_mismatch: bool },
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

fn validate_selector(selector: &str) -> Result<()> {
    let raw = selector
        .strip_prefix("0x")
        .ok_or_else(|| anyhow!("selector must start with 0x"))?;
    if raw.len() != 8 || !raw.as_bytes().iter().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "selector must be 4 bytes of hex (expected 0xXXXXXXXX)"
        ));
    }
    Ok(())
}

fn deserialize_tx_for_validation(bytes: &[u8], field_name: &str) -> Result<Transaction> {
    bincode::deserialize::<Transaction>(bytes)
        .with_context(|| format!("{field_name} does not deserialize with current Transaction layout"))
}

fn validate_fixture_file(path: &PathBuf, expected_program_id: [u8; 32]) -> Result<()> {
    let raw = fs::read(path).with_context(|| format!("read fixture {}", path.display()))?;
    let fixture: FixtureInput = serde_json::from_slice(&raw)
        .with_context(|| format!("parse fixture JSON {}", path.display()))?;

    if fixture.format != FIXTURE_FORMAT {
        return Err(anyhow!(
            "unsupported fixture format {:?}; expected {:?}",
            fixture.format,
            FIXTURE_FORMAT
        ));
    }

    validate_selector(&fixture.selector)?;

    let tx_bytes = BASE64
        .decode(&fixture.tx_b64)
        .context("decode fixture tx_b64 from base64")?;
    let tx_tampered_bytes = BASE64
        .decode(&fixture.tx_tampered_b64)
        .context("decode fixture tx_tampered_b64 from base64")?;

    let tx = deserialize_tx_for_validation(&tx_bytes, "tx_b64")?;
    let tx_tampered = deserialize_tx_for_validation(&tx_tampered_bytes, "tx_tampered_b64")?;

    if tx.actions.is_empty() {
        return Err(anyhow!("tx_b64 deserialized to empty transaction"));
    }
    if tx_tampered.actions.is_empty() {
        return Err(anyhow!("tx_tampered_b64 deserialized to empty transaction"));
    }

    let tx_proof = tx
        .aggregation_proof
        .as_ref()
        .ok_or_else(|| anyhow!("tx_b64 is missing aggregation_proof"))?;
    Seal::try_from_slice(tx_proof)
        .context("tx_b64 aggregation_proof is not a valid verifier_router Seal")?;

    let tx_tampered_proof = tx_tampered
        .aggregation_proof
        .as_ref()
        .ok_or_else(|| anyhow!("tx_tampered_b64 is missing aggregation_proof"))?;
    Seal::try_from_slice(tx_tampered_proof)
        .context("tx_tampered_b64 aggregation_proof is not a valid verifier_router Seal")?;

    if fixture.consumed_nullifiers_b64.is_empty() {
        return Err(anyhow!("fixture must include consumed_nullifiers_b64 entries"));
    }
    for (idx, nf_b64) in fixture.consumed_nullifiers_b64.iter().enumerate() {
        let bytes = BASE64
            .decode(nf_b64)
            .with_context(|| format!("decode consumed_nullifiers_b64[{idx}]"))?;
        if bytes.len() != 32 {
            return Err(anyhow!(
                "consumed_nullifiers_b64[{idx}] must decode to 32 bytes, got {}",
                bytes.len()
            ));
        }
    }

    if !tx_bytes
        .windows(expected_program_id.len())
        .any(|w| w == expected_program_id.as_slice())
    {
        return Err(anyhow!(
            "transaction bytes do not contain expected program ID bytes"
        ));
    }

    Ok(())
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


const SPL_TOKEN_FORWARDER_PROGRAM_ID: &str = "3cLKSYBijunpCc2F2gzizUkhYtyFrLr4RVdNiaK79b48";

fn sha256_hash(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    Sha256Digest::update(&mut hasher, data);
    let result = hasher.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&result);
    arr
}

fn spl_token_forwarder_wrap_external_payload(
    action_tree_root: &Digest,
    output_mismatch: bool,
) -> Result<(ExpirableBlob, SplTokenWrapMetadata)> {
    let program_id = decode_base58_32(SPL_TOKEN_FORWARDER_PROGRAM_ID)?;

    let seed = sha256_hash(b"spl_token_forwarder_test_user");
    let signing_key = SigningKey::from_bytes(&seed);
    let user_pubkey = signing_key.verifying_key().to_bytes();

    // Deterministic token mint keypair — test recreates via Keypair.fromSeed(mint_seed)
    let mint_seed = sha256_hash(b"spl_token_forwarder_test_mint");
    let mint_signing_key = SigningKey::from_bytes(&mint_seed);
    let token_mint: [u8; 32] = mint_signing_key.verifying_key().to_bytes();

    let amount: u64 = 100_000_000; // 100 tokens (6 decimals)
    let nonce: u64 = 1;
    let deadline: i64 = 4_102_444_800; // year 2100
    let action_tree_root_bytes = action_tree_root.as_bytes();

    // SHA256(forwarder_id || token_mint || amount || nonce || deadline || action_tree_root)
    // forwarder_id provides domain separation (like EIP-712 verifyingContract)
    let mut message = Vec::with_capacity(120);
    message.extend_from_slice(&program_id);
    message.extend_from_slice(&token_mint);
    message.extend_from_slice(&amount.to_le_bytes());
    message.extend_from_slice(&nonce.to_le_bytes());
    message.extend_from_slice(&deadline.to_le_bytes());
    message.extend_from_slice(action_tree_root_bytes);

    let message_hash = sha256_hash(&message);
    // The on-chain forwarder base64-encodes the hash before comparing to the
    // Ed25519 instruction's message. The Ed25519 program verifies the signature
    // against the instruction's message. So the signature must be over base64(hash).
    let b64_message = BASE64.encode(message_hash);
    let signature = signing_key.sign(b64_message.as_bytes());
    let signature_bytes = signature.to_bytes();

    // Logic ref must match what's in the transaction (PASSTHROUGH_LOGIC_GUEST_ID)
    let logic_ref: [u8; 32] = {
        let digest: risc0_zkvm::sha::Digest = PASSTHROUGH_LOGIC_GUEST_ID.into();
        digest.as_bytes().try_into().unwrap()
    };

    // Wrap input: op(1) + token_mint(32) + amount(8) + user(32) + nonce(8) + deadline(8) + action_tree_root(32) + signature(64) + ed25519_ix_index(1) = 186 bytes
    let mut input = Vec::with_capacity(186);
    input.push(OP_WRAP);
    input.extend_from_slice(&token_mint);
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&user_pubkey);
    input.extend_from_slice(&nonce.to_le_bytes());
    input.extend_from_slice(&deadline.to_le_bytes());
    input.extend_from_slice(action_tree_root_bytes);
    input.extend_from_slice(&signature_bytes);
    input.push(0); // ed25519_ix_index = 0

    let expected_output = if output_mismatch {
        vec![0x00]
    } else {
        vec![SPL_RESULT_SUCCESS]
    };

    let blob = encode_external_call(&SolanaExternalCall {
        program_id,
        instruction_data: input,
        expected_output,
        output_mode: OutputMode::ReturnData,
    });

    let metadata = SplTokenWrapMetadata {
        user_secret_key_b64: BASE64.encode(seed),
        user_pubkey_b64: BASE64.encode(user_pubkey),
        mint_seed_b64: BASE64.encode(mint_seed),
        token_mint_b58: bs58::encode(&token_mint).into_string(),
        amount,
        nonce,
        deadline,
        action_tree_root_b64: BASE64.encode(action_tree_root_bytes),
        signature_b64: BASE64.encode(signature_bytes),
        logic_ref_b64: BASE64.encode(logic_ref),
    };

    Ok((blob, metadata))
}

fn spl_token_forwarder_unwrap_external_payload(
    output_mismatch: bool,
) -> Result<(ExpirableBlob, SplTokenUnwrapMetadata)> {
    let program_id = decode_base58_32(SPL_TOKEN_FORWARDER_PROGRAM_ID)?;

    let mint_seed = sha256_hash(b"spl_token_forwarder_test_mint");
    let mint_signing_key = SigningKey::from_bytes(&mint_seed);
    let token_mint: [u8; 32] = mint_signing_key.verifying_key().to_bytes();

    let recipient_seed = sha256_hash(b"spl_token_forwarder_test_recipient");
    let recipient_signing_key = SigningKey::from_bytes(&recipient_seed);
    let recipient: [u8; 32] = recipient_signing_key.verifying_key().to_bytes();

    let logic_ref: [u8; 32] = {
        let digest: risc0_zkvm::sha::Digest = PASSTHROUGH_LOGIC_GUEST_ID.into();
        digest.as_bytes().try_into().unwrap()
    };

    let amount: u64 = 50_000_000; // 50 tokens

    // Unwrap input: op(1) + token_mint(32) + amount(8) + recipient(32) = 73 bytes
    let mut input = Vec::with_capacity(73);
    input.push(OP_UNWRAP);
    input.extend_from_slice(&token_mint);
    input.extend_from_slice(&amount.to_le_bytes());
    input.extend_from_slice(&recipient);

    let expected_output = if output_mismatch {
        vec![0x00]
    } else {
        vec![SPL_RESULT_SUCCESS]
    };

    let blob = encode_external_call(&SolanaExternalCall {
        program_id,
        instruction_data: input,
        expected_output,
        output_mode: OutputMode::ReturnData,
    });

    let metadata = SplTokenUnwrapMetadata {
        mint_seed_b64: BASE64.encode(mint_seed),
        token_mint_b58: bs58::encode(&token_mint).into_string(),
        amount,
        recipient_seed_b64: BASE64.encode(recipient_seed),
        recipient_b58: bs58::encode(&recipient).into_string(),
        logic_ref_b64: BASE64.encode(logic_ref),
    };

    Ok((blob, metadata))
}

struct TransactionGenerationResult {
    tx: Transaction,
    spl_token_wrap_metadata: Option<SplTokenWrapMetadata>,
    spl_token_unwrap_metadata: Option<SplTokenUnwrapMetadata>,
}

fn generate_test_transaction_with_external_payload(
    forwarder_mode: ForwarderMode,
    nonce_seed: Option<u8>,
    multi_external_call: bool,
) -> Result<TransactionGenerationResult> {
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
        } | ForwarderMode::SplTokenWrap {
            output_mismatch: true
        } | ForwarderMode::SplTokenUnwrap {
            output_mismatch: true
        }
    );
    let nonce_byte: u8 = nonce_seed.unwrap_or(match (&forwarder_mode, output_mismatch) {
        (ForwarderMode::SplTokenWrap { .. }, false) => 10,
        (ForwarderMode::SplTokenWrap { .. }, true) => 12,
        (ForwarderMode::SplTokenUnwrap { .. }, false) => 20,
        (ForwarderMode::SplTokenUnwrap { .. }, true) => 22,
        (_, true) => 2,
        (_, false) => 0,
    });
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

    // Create app_data with external payload based on forwarder type.
    let mut consumed_app_data = AppData::default();
    let mut spl_token_wrap_metadata = None;
    let mut spl_token_unwrap_metadata = None;

    match &forwarder_mode {
        ForwarderMode::BlockTimeForwarder { output_mismatch } => {
            consumed_app_data
                .external_payload
                .push(block_time_forwarder_external_payload_blob(*output_mismatch)?);
        }
        ForwarderMode::TestForwarderFail => {
            consumed_app_data
                .external_payload
                .push(test_forwarder_fail_payload_blob()?);
        }
        ForwarderMode::TestForwarderSilent => {
            consumed_app_data
                .external_payload
                .push(test_forwarder_silent_payload_blob()?);
        }
        ForwarderMode::TestForwarderOutputAccount => {
            consumed_app_data
                .external_payload
                .push(test_forwarder_output_account_payload_blob(b"\x01\x02\x03\x04", 2)?);
        }
        ForwarderMode::SplTokenWrap { output_mismatch } => {
            let (blob, metadata) =
                spl_token_forwarder_wrap_external_payload(&root, *output_mismatch)?;
            consumed_app_data.external_payload.push(blob);
            spl_token_wrap_metadata = Some(metadata);
        }
        ForwarderMode::SplTokenUnwrap { output_mismatch } => {
            let (blob, metadata) = spl_token_forwarder_unwrap_external_payload(*output_mismatch)?;
            consumed_app_data.external_payload.push(blob);
            spl_token_unwrap_metadata = Some(metadata);
        }
    }
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

    Ok(TransactionGenerationResult {
        tx: balanced_tx,
        spl_token_wrap_metadata,
        spl_token_unwrap_metadata,
    })
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
            format: FIXTURE_FORMAT,
            aggregation_strategy: "batch",
            aggregation_proof_type: "groth16",
            selector: selector.to_owned(),
            forwarder_type: "block_time",
            tx_b64: BASE64.encode(tx_bytes),
            tx_tampered_b64: String::new(),
            consumed_nullifiers_b64: nullifiers_b64.to_vec(),
            spl_token_wrap: None,
            spl_token_unwrap: None,
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
            let nf = cu.instance.consumed_nullifier.to_bytes();
            let cm = cu.instance.created_commitment.to_bytes();
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
        "Usage:\n  fixture-gen [OPTIONS] [OUT_PATH]\n  fixture-gen --validate FIXTURE_PATH --program-id PROGRAM_ID_B58\n  fixture-gen strip-calls <IN> <OUT>       Remove external calls from a fixture\n  fixture-gen dump <IN>                    Print transaction structure\n\nForwarder types:\n  - (default) BlockTime forwarder\n  - --spl-token-wrap SPL Token wrap with Ed25519 signature\n  - --spl-token-unwrap SPL Token unwrap (escrow release)\n\nNotes:\n  - --threads N sets rayon thread pool size\n  - --validate checks fixture deserialization and program-id binding\n  - At most one forwarder mode flag may be set\n"
    );
}

fn parse_validate_args(args: impl Iterator<Item = String>) -> Result<CliCommand> {
    let mut args = args;
    let mut fixture_path: Option<PathBuf> = None;
    let mut program_id_b58: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--program-id" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow!("--program-id requires a value"))?;
                program_id_b58 = Some(value);
            }
            _ if arg.starts_with("--program-id=") => {
                let value = arg
                    .split_once('=')
                    .map(|(_, v)| v)
                    .ok_or_else(|| anyhow!("--program-id requires a value"))?;
                program_id_b58 = Some(value.to_string());
            }
            _ if arg.starts_with('-') => {
                return Err(anyhow!("unknown flag in validate mode: {arg}"));
            }
            _ => {
                if fixture_path.is_some() {
                    return Err(anyhow!("unexpected extra argument in validate mode: {arg}"));
                }
                fixture_path = Some(PathBuf::from(arg));
            }
        }
    }

    let fixture_path = fixture_path.ok_or_else(|| anyhow!("--validate requires FIXTURE_PATH"))?;
    let program_id_b58 =
        program_id_b58.ok_or_else(|| anyhow!("--validate requires --program-id PROGRAM_ID_B58"))?;
    let program_id = decode_base58_32(&program_id_b58).context("invalid --program-id")?;

    Ok(CliCommand::Validate {
        fixture_path,
        program_id,
    })
}

fn parse_generate_args(mut args: impl Iterator<Item = String>) -> Result<CliArgs> {
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
            | "--forwarder-output-account" | "--spl-token-wrap" | "--spl-token-unwrap" => {
                if forwarder_mode.is_some() {
                    return Err(anyhow!(
                        "at most one forwarder mode flag may be set"
                    ));
                }
                forwarder_mode = Some(match flag {
                    "--output-mismatch" => ForwarderMode::BlockTimeForwarder {
                        output_mismatch: true,
                    },
                    "--forwarder-fail" => ForwarderMode::TestForwarderFail,
                    "--forwarder-silent" => ForwarderMode::TestForwarderSilent,
                    "--forwarder-output-account" => ForwarderMode::TestForwarderOutputAccount,
                    "--spl-token-wrap" => ForwarderMode::SplTokenWrap {
                        output_mismatch: false,
                    },
                    "--spl-token-unwrap" => ForwarderMode::SplTokenUnwrap {
                        output_mismatch: false,
                    },
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

    let forwarder_mode = forwarder_mode.unwrap_or(ForwarderMode::BlockTimeForwarder {
        output_mismatch: false,
    });

    let out_path = out_path.unwrap_or_else(|| {
        let filename = match &forwarder_mode {
            ForwarderMode::SplTokenWrap { .. } => "spl_token_wrap.json",
            ForwarderMode::SplTokenUnwrap { .. } => "spl_token_unwrap.json",
            _ => "batch_groth16.json",
        };
        PathBuf::from(format!("solana-pa-prototype/tests/fixtures/{filename}"))
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

fn parse_args() -> Result<CliCommand> {
    let mut args = env::args().skip(1);
    let Some(first) = args.next() else {
        return Ok(CliCommand::Generate(parse_generate_args(std::iter::empty())?));
    };

    match first.as_str() {
        "-h" | "--help" => {
            print_usage();
            std::process::exit(0);
        }
        "--validate" => parse_validate_args(args),
        "strip-calls" => {
            let args: Vec<String> = args.collect();
            if args.len() != 2 {
                return Err(anyhow!(
                    "Usage: fixture-gen strip-calls <input.json> <output.json>"
                ));
            }
            Ok(CliCommand::StripCalls {
                input: PathBuf::from(&args[0]),
                output: PathBuf::from(&args[1]),
            })
        }
        "dump" => {
            let input = args.next().ok_or_else(|| {
                anyhow!("Usage: fixture-gen dump <input.json>")
            })?;
            Ok(CliCommand::Dump {
                input: PathBuf::from(input),
            })
        }
        _ => {
            let generate_args = std::iter::once(first).chain(args);
            Ok(CliCommand::Generate(parse_generate_args(generate_args)?))
        }
    }
}

fn main() -> Result<()> {
    let command = parse_args()?;

    let args = match command {
        CliCommand::Validate {
            fixture_path,
            program_id,
        } => {
            validate_fixture_file(&fixture_path, program_id)?;
            eprintln!("fixture validation OK: {}", fixture_path.display());
            return Ok(());
        }
        CliCommand::StripCalls { input, output } => {
            return strip_calls_from_fixture(&input, &output);
        }
        CliCommand::Dump { input } => {
            return dump_fixture(&input);
        }
        CliCommand::Generate(args) => args,
    };

    let CliArgs {
        threads,
        debug_assumptions,
        forwarder_mode,
        nonce_seed,
        multi_external_call,
        error_variants_dir,
        out_path,
    } = args;

    let total_start = Instant::now();

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

    let forwarder_type_str = match &forwarder_mode {
        ForwarderMode::BlockTimeForwarder { .. } => "block_time",
        ForwarderMode::TestForwarderFail
        | ForwarderMode::TestForwarderSilent
        | ForwarderMode::TestForwarderOutputAccount => "test_forwarder",
        ForwarderMode::SplTokenWrap { .. } => "spl_token_wrap",
        ForwarderMode::SplTokenUnwrap { .. } => "spl_token_unwrap",
    };

    eprintln!("fixture output: {}", out_path.display());
    eprintln!("forwarder: {forwarder_type_str}");
    eprintln!("mode: aggregated (batch Groth16)");
    if let ForwarderMode::BlockTimeForwarder {
        output_mismatch: true,
    }
    | ForwarderMode::SplTokenWrap {
        output_mismatch: true,
    }
    | ForwarderMode::SplTokenUnwrap {
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

    let gen_result = timed_phase("generate_test_transaction", || {
        generate_test_transaction_with_external_payload(forwarder_mode, nonce_seed, multi_external_call)
    })?;
    let mut tx = gen_result.tx;
    let spl_token_wrap_metadata = gen_result.spl_token_wrap_metadata;
    let spl_token_unwrap_metadata = gen_result.spl_token_unwrap_metadata;

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
        format: FIXTURE_FORMAT,
        aggregation_strategy: "batch",
        aggregation_proof_type: "groth16",
        selector,
        forwarder_type: forwarder_type_str,
        tx_b64: BASE64.encode(tx_bytes),
        tx_tampered_b64: BASE64.encode(tx_tampered_bytes),
        consumed_nullifiers_b64,
        spl_token_wrap: spl_token_wrap_metadata,
        spl_token_unwrap: spl_token_unwrap_metadata,
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
