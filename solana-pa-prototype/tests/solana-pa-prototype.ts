import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
  ComputeBudgetProgram,
  Ed25519Program,
  SYSVAR_CLOCK_PUBKEY,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import {
  approve,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import path from "path";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";

import {
  getRouterPda,
  getVerifierEntryPda,
  VERIFIER_ROUTER_ID,
  verifierForSelector,
} from "../scripts/verifier-utils";

import {
  PA_STATE_SEED,
  TX_DATA_SEED,
  EMPTY_TREE_ROOT_INITIAL,
  EMPTY_KIND_TABLE_COMMITMENT,
  MIN_EXPIRY_SLOTS,
  MAX_EXPIRY_SLOTS,
  SEVEN_DAYS_SLOTS,
  AUTHORITY_MISMATCH_PATTERN,
  SEED_MISMATCH_PATTERN,
  ADDRESS_MISMATCH_PATTERN,
  readJson,
  loadFixture,
  type Fixture,
  parseSelectorFromFixture,
  createdCommitmentsOf,
  fundKeypair,
  drainKeypairs,
  paInitializeBuilder,
  uploadTxData as uploadTxDataTo,
  computeRootAfterAppend,
  deriveNullifierAccounts as deriveNullifierAccountsFromB64,
  deriveProgramDataPda,
  deriveRootMarkerPda,
  EMERGENCY_COMMITTEE_LABEL,
  createWrapMessageHash,
  deriveConfigPda,
  deriveEscrowPda,
  deriveNonceBitmapPda,
  isNonceUsed,
  regenerateCommand,
  seededKeypair,
} from "./utils";

// Keypairs funded during tests, drained back to the provider wallet in
// afterEach() so devnet SOL circulates across the test run.
const fundedKeypairs: Keypair[] = [];

// TxData accounts created during tests, closed in afterEach() to recover rent.
const openTxDataAccounts: { uploadId: anchor.BN; txData: PublicKey; authority: Keypair }[] = [];

let providerBalanceBefore = 0;
let suiteStartBalance = 0;

async function airdrop(provider: anchor.AnchorProvider, kp: Keypair, sol: number) {
  await fundKeypair(provider, kp, sol);
  fundedKeypairs.push(kp);
}

const IDL_PATH = path.resolve(process.cwd(), "target", "idl", "protocol_adapter.json");

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);

const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);

const programData = deriveProgramDataPda(program.programId);

const fixture = loadFixture("batch_groth16.json");

const PROOF_SELECTOR = parseSelectorFromFixture(fixture.selector);
const VERIFIER = verifierForSelector(PROOF_SELECTOR);
const VERIFIER_PROGRAM_ID = VERIFIER.program;

const [routerPda] = getRouterPda(VERIFIER_ROUTER_ID);
const [verifierEntryPda] = getVerifierEntryPda(PROOF_SELECTOR, VERIFIER_ROUTER_ID);

// Must match `programs/block-time-forwarder/src/lib.rs::declare_id!`.
const blockTimeForwarderId = new PublicKey("3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf");

// Must match `programs/test-forwarder/src/lib.rs::declare_id!`.
const testForwarderId = new PublicKey("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD");

function deriveRootPda(root: Buffer): PublicKey {
  return deriveRootMarkerPda(paState, root, program.programId);
}

// `newRootMarker` is a required named account, so every settle/settleFromTxdata
// call needs a value even when the test expects settlement to fail before the
// account is ever read. `initialize` never creates a root marker for the
// empty-tree root — `is_root_valid` accepts it directly, without a marker —
// so this PDA never exists on-chain. It's used purely as a syntactically
// valid placeholder address; its value is irrelevant for those tests since
// the instruction fails earlier.
const DUMMY_ROOT_MARKER = deriveRootPda(EMPTY_TREE_ROOT_INITIAL);

async function predictRootMarkerPda(createdCommitments: Buffer[]): Promise<PublicKey> {
  const state = await program.account.paStateAccount.fetch(paState);
  const root = computeRootAfterAppend(state, createdCommitments);
  return deriveRootPda(root);
}

function deriveNullifierAccounts(nullifierB64s: string[]): { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] {
  return deriveNullifierAccountsFromB64(nullifierB64s, paState, program.programId);
}

function buildInitialize(payer: PublicKey) {
  return paInitializeBuilder(program, payer, PROOF_SELECTOR);
}

/**
 * Assert that `fixtureName` has not already been settled on this validator.
 *
 * Every test that settles a fixture asserts an exact state transition
 * (next_index before -> after, a specific resulting root, specific markers).
 * Those assertions are only meaningful on a fresh ledger. If a fixture's
 * nullifiers already exist, the ledger is not fresh and the rest of the test is
 * measuring something else.
 *
 * This fails loudly rather than skipping or degrading to a weaker check: a
 * silently retired test reports as pending, which reads as green, and a
 * weakened one reports as passing while verifying materially less.
 */
async function assertFixtureUnsettled(fixtureName: string): Promise<void> {
  const f = loadFixture(fixtureName);
  const nullifierAccounts = deriveNullifierAccounts(f.consumed_nullifiers_b64);
  const info = await provider.connection.getAccountInfo(nullifierAccounts[0].pubkey);
  assert.isNull(
    info,
    `${fixtureName} is already settled on this validator (its first nullifier ` +
    `marker exists). These tests require a fresh ledger. Reset it with ` +
    `'./scripts/dev.sh clean' and re-run, or deploy to a fresh devnet.`
  );
}

function buildSettleRemainingAccounts(
  nullifierAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
  options?: {
    additionalHistoricalRootMarkers?: PublicKey[];
  }
): { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] {
  const accounts = [
    ...nullifierAccounts,
    { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
    { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
  ];
  if (options?.additionalHistoricalRootMarkers) {
    for (const marker of options.additionalHistoricalRootMarkers) {
      accounts.push({ pubkey: marker, isWritable: false, isSigner: false });
    }
  }
  return accounts;
}

async function uploadTxData(
  authority: Keypair,
  payload: Buffer,
  expiresSlotOverride?: anchor.BN
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey }> {
  const upload = await uploadTxDataTo(program, paState, authority, payload, expiresSlotOverride);
  openTxDataAccounts.push({ uploadId: upload.uploadId, txData: upload.txData, authority });
  return upload;
}

function freshUploadId(): { uploadId: anchor.BN; uploadIdLe: Buffer } {
  const uploadId = new anchor.BN(Date.now());
  const uploadIdLe = Buffer.alloc(8);
  uploadIdLe.writeBigUInt64LE(BigInt(uploadId.toString()));
  return { uploadId, uploadIdLe };
}

async function initTxData(
  authority: Keypair,
  payloadSize: number,
  expiresSlotOverride?: anchor.BN,
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey; expiresSlot: anchor.BN }> {
  const { uploadId, uploadIdLe } = freshUploadId();
  const [txData] = PublicKey.findProgramAddressSync(
    [TX_DATA_SEED, authority.publicKey.toBuffer(), uploadIdLe],
    program.programId
  );
  const expiresSlot = expiresSlotOverride ??
    new anchor.BN((await provider.connection.getSlot("confirmed")) + 10_000);
  await program.methods
    .txdataInit(uploadId, payloadSize, expiresSlot)
    .accountsPartial({
      paState,
      txData,
      authority: authority.publicKey,
      systemProgram: SystemProgram.programId,
    })
    .signers([authority])
    .rpc();
  openTxDataAccounts.push({ uploadId, txData, authority });
  return { uploadId, uploadIdLe, txData, expiresSlot };
}

// Anchor assigns 6000 + enum_variant_index.
const PA_ERRORS: Record<string, number> = Object.fromEntries(
  (readJson<{ errors?: { name: string; code: number }[] }>(IDL_PATH).errors ?? [])
    .map((e) => [e.name, e.code]),
);

const PA_ERROR_NAMES = new Map(Object.entries(PA_ERRORS).map(([k, v]) => [v, k]));

// For CPI errors, Solana propagates the inner program's error code —
// the PA's failure line shows the inner code, not the PA's own error.
function extractPAErrorCode(e: any): number | null {
  const logs: string[] = e?.logs ?? e?.error?.logs ?? [];
  const paId = program.programId.toBase58();
  // Find the PA's own failure line (not inner CPI programs)
  for (let i = logs.length - 1; i >= 0; i--) {
    if (!logs[i].includes(paId)) continue;
    const match = logs[i].match(/failed: custom program error: 0x([0-9a-fA-F]+)/);
    if (match) return parseInt(match[1], 16);
  }
  return null;
}

function assertPAError(e: any, errorName: string): void {
  const expectedCode = PA_ERRORS[errorName];
  assert.isDefined(expectedCode, `Unknown PA error name: ${errorName}`);

  const actualCode = extractPAErrorCode(e);
  const actualName = actualCode !== null ? PA_ERROR_NAMES.get(actualCode) : null;
  const logs: string[] = e?.logs ?? e?.error?.logs ?? [];

  assert.strictEqual(
    actualCode,
    expectedCode,
    `Expected PA error ${errorName} (${expectedCode}), ` +
      `got ${actualName ?? "unknown"} (${actualCode})` +
      `\nLogs:\n${logs.slice(-15).join("\n")}`,
  );
}

// For Anchor framework constraint errors where the error is in log format
// rather than a numeric program error code.
function errorHaystack(e: any): string {
  const parts: string[] = [];
  if (e?.message) parts.push(e.message);
  if (e?.error?.errorMessage) parts.push(e.error.errorMessage);
  if (e?.error?.errorCode?.code) parts.push(e.error.errorCode.code);
  const logs: string[] = e?.logs ?? e?.error?.logs ?? [];
  parts.push(...logs);
  return parts.join("\n");
}

async function waitForSlotPast(
  connection: anchor.web3.Connection,
  targetSlot: number,
  timeoutMs: number = 30000
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const slot = await connection.getSlot("confirmed");
    if (slot > targetSlot) return;
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error(`Timed out waiting for slot past ${targetSlot} after ${timeoutMs}ms`);
}

/** Anchor's CPI event tag: the fixed 8-byte `EVENT_IX_TAG_LE`, the little-endian encoding of the u64 0x1d9acb512ea545e4. */
const EVENT_IX_TAG_LE = Buffer.from([0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d]);

/**
 * Settlement events are CPI events: inner instructions of the adapter whose
 * data is Anchor's event tag followed by the event's discriminator and Borsh
 * body. Read them in emission order from the confirmed transaction.
 */
function parseCpiEvents(tx: anchor.web3.VersionedTransactionResponse) {
  const keys = tx.transaction.message.getAccountKeys({
    accountKeysFromLookups: tx.meta?.loadedAddresses,
  });
  const coder = new anchor.BorshCoder(program.idl);
  const events: { name: string; data: any }[] = [];
  for (const group of tx.meta?.innerInstructions ?? []) {
    for (const ix of group.instructions) {
      if (!keys.get(ix.programIdIndex)?.equals(program.programId)) continue;
      const data = Buffer.from(anchor.utils.bytes.bs58.decode(ix.data));
      if (data.length < 16 || !data.subarray(0, 8).equals(EVENT_IX_TAG_LE)) continue;
      const decoded = coder.events.decode(data.subarray(8).toString("base64"));
      if (decoded) events.push(decoded);
    }
  }
  return events;
}

function settleFromTxDataBuilder(
  authority: PublicKey,
  uploadId: anchor.BN,
  txData: PublicKey,
  newRootMarker: PublicKey,
  remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
) {
  return program.methods
    .settleFromTxdata(uploadId)
    .accountsPartial({
      paState,
      txData,
      authority,
      systemProgram: SystemProgram.programId,
      newRootMarker,
      verifierRouterProgram: VERIFIER_ROUTER_ID,
      router: routerPda,
      verifierEntry: verifierEntryPda,
      verifierProgram: VERIFIER_PROGRAM_ID,
    })
    .remainingAccounts(remainingAccounts)
    .preInstructions([
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
      ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ]);
}

async function settleFixtureViaTxData(
  payload: Buffer,
  remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
  options?: { newRootMarker?: PublicKey; createdCommitments?: Buffer[] },
): Promise<string> {
  const authority = Keypair.generate();
  await airdrop(provider, authority, 2);
  const { uploadId, txData } = await uploadTxData(authority, payload);

  const newRootMarker =
    options?.newRootMarker ??
    (await predictRootMarkerPda(requireCommitments(options?.createdCommitments)));
  return settleFromTxDataBuilder(authority.publicKey, uploadId, txData, newRootMarker, remainingAccounts)
    .signers([authority])
    .rpc();
}

// A settlement expected to succeed must predict its produced-root marker from
// the transaction's created commitments; one expected to fail passes an
// explicit (dummy) marker instead.
function requireCommitments(createdCommitments?: Buffer[]): Buffer[] {
  assert.ok(
    createdCommitments,
    "settlement without an explicit newRootMarker needs createdCommitments to predict it",
  );
  return createdCommitments!;
}

function commitmentsOf(fx: Fixture): Buffer[] {
  return createdCommitmentsOf(fx);
}

async function paStateExists(): Promise<boolean> {
  const info = await provider.connection.getAccountInfo(paState);
  return info !== null;
}

describe("protocol-adapter (AUTH-01: initialization authority)", () => {
  it("rejects initialization by a non-upgrade-authority signer", async () => {
    // Must run before PAState is initialized anywhere else in the suite —
    // otherwise the `init` constraint on `pa_state` would fail with
    // "already in use" before the AUTH-01 constraint on `program_data` is
    // ever reached, which would prove nothing about this fix.
    assert.isFalse(
      await paStateExists(),
      "PAState was already initialized before the AUTH-01 rejection test ran; " +
        "this test must execute first so it observes an uninitialized state"
    );

    const stranger = Keypair.generate();
    await airdrop(provider, stranger, 2);

    let caught: any = null;
    try {
      await buildInitialize(stranger.publicKey).signers([stranger]).rpc();
    } catch (e: any) {
      caught = e;
    }
    assert.isNotNull(
      caught,
      "expected initialization by a non-upgrade-authority signer to fail"
    );

    // The error must be our Unauthorized code, and it must have been raised by
    // the `program_data` account's upgrade-authority constraint specifically —
    // not by account resolution, not by the `program` constraint, and not by
    // any earlier check. Anchor names the offending account in its log line,
    // which is what distinguishes "rejected for the right reason" from
    // "rejected before the constraint was ever evaluated".
    assertPAError(caught, "Unauthorized");
    assert.match(
      errorHaystack(caught),
      /AnchorError caused by account: program_data/,
      "Unauthorized must originate from the program_data upgrade-authority " +
        `constraint. Got:\n${errorHaystack(caught)}`
    );

    // The rejected transaction must not have left PAState initialized.
    assert.isFalse(
      await paStateExists(),
      "PAState must remain uninitialized after the rejected call"
    );
  });
});

describe("protocol-adapter (Groth16 batch aggregation E2E)", () => {
  const tx = Buffer.from(fixture.tx_b64, "base64");
  const txTampered = Buffer.from(fixture.tx_tampered_b64, "base64");

  const remainingAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
  const nullifierPdas = remainingAccounts.map((a) => a.pubkey);

  async function settleViaTxData(
    authority: Keypair,
    payload: Buffer,
    options?: {
      nullifierAccounts?: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[];
      newRootMarker?: PublicKey;
      createdCommitments?: Buffer[];
      additionalHistoricalRootMarkers?: PublicKey[];
    }
  ) {
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await uploadTxData(authority, payload);

    const allRemainingAccounts = buildSettleRemainingAccounts(
      options?.nullifierAccounts ?? remainingAccounts,
      options
    );

    const buildSettle = (newRootMarker: PublicKey) =>
      settleFromTxDataBuilder(
        authority.publicKey,
        uploadId,
        txData,
        newRootMarker,
        allRemainingAccounts
      ).signers([authority]);

    const newRootMarker =
      options?.newRootMarker ??
      (await predictRootMarkerPda(requireCommitments(options?.createdCommitments)));
    return buildSettle(newRootMarker).rpc();
  }

  before(async () => {
    try {
      await program.account.paStateAccount.fetch(paState);
    } catch {
      await buildInitialize(provider.wallet.publicKey).rpc();
    }
  });

  it("initializes with depth 1 (variable-depth tree)", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      state.schemaVersion,
      1,
      "a freshly initialized adapter carries schema version 1 (PAStateAccount::SCHEMA_VERSION)"
    );
    assert.isAtLeast(
      state.currentDepth,
      1,
      "Tree depth should be at least 1"
    );
    assert.equal(
      state.frontier.length,
      state.currentDepth,
      "Frontier length should equal current depth"
    );
    if (state.nextIndex.toNumber() === 0) {
      // Fresh PA: root should be genesis
      const rootBytes = Buffer.from(state.root as number[]);
      assert.deepEqual(
        rootBytes,
        EMPTY_TREE_ROOT_INITIAL,
        "Initial root should be ZEROS[0] for depth-1 tree"
      );
    }
  });

  it("account size matches expected size for current depth (no over-allocation)", async () => {
    // Space formula: BASE_SPACE (201) + VEC_OVERHEAD (4) + 32 * depth
    // BASE_SPACE breakdown matches state.rs: discriminator(8) +
    // schema_version(1) + bump(1) + authority(32) + verifier_router(32) +
    // proof_selector(4) + kind_table_commitment(32) + pending_authority(33) +
    // lifecycle(1) + root(32) + next_index(8) + current_depth(1) +
    // expiry bounds(16)
    const BASE_SPACE = 201;
    const VEC_OVERHEAD = 4;
    const spaceForDepth = (depth: number) => BASE_SPACE + VEC_OVERHEAD + 32 * depth;

    const state = await program.account.paStateAccount.fetch(paState);
    const accountInfo = await provider.connection.getAccountInfo(paState);

    assert.ok(accountInfo, "PAState account should exist");

    const expectedSize = spaceForDepth(state.currentDepth);
    assert.equal(
      accountInfo!.data.length,
      expectedSize,
      `Account size (${accountInfo!.data.length}) should match expected size for depth ${state.currentDepth} (${expectedSize})`
    );
  });

  it("rejects Delta::Witness (balance conservation bypass attempt)", async () => {
    // A WELL-FORMED transaction carrying Delta::Witness — the fixture's
    // aggregated transaction with its delta proof replaced by the actual
    // delta witness (fixture-gen's witness_delta.json error variant). It
    // deserializes cleanly, so the rejection must come from the PA's own
    // witness check — a clean ExpectedDeltaProof, never a crash. A witness
    // scalar is prover-side private data; deserializing it on-chain must
    // never execute curve arithmetic (the k256 stack-overflow class).
    const fx = loadFixture("witness_delta.json");
    const txWitness = Buffer.from(fx.tx_b64, "base64");

    try {
      await settleViaTxData(Keypair.generate(), txWitness, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected settle to fail");
    } catch (e: any) {
      assertPAError(e, "ExpectedDeltaProof");
    }
  });

  it("rejects a tampered tx (proof binding)", async () => {
    try {
      await settleViaTxData(Keypair.generate(), txTampered, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected settle to fail");
    } catch (e: any) {
      // The PA calls the verifier router via CPI, which calls the verifier the
      // fixture's selector routes to. Solana's CPI error propagation records
      // the INNER program's error code in the PA's failure line — so we see
      // the verifier's code instead of the PA's VerifierRouterFailed (6013).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.equal(
        code,
        VERIFIER.rejectionCode,
        `the fixture selector's verifier rejection code (${VERIFIER.rejectionCode}) should propagate through CPI`,
      );
    }
  });

  it("accepts a valid Groth16 batch aggregation tx and creates root marker", async () => {
    // Guardrail: fixture should actually include a block-time-forwarder external call.
    // If not present, this test can pass without exercising the external call path.
    assert.ok(
      tx.includes(Buffer.from(blockTimeForwarderId.toBytes())),
      "fixture tx must include block-time-forwarder program id bytes (external_payload injected)"
    );

    // Requires a fresh ledger: the assertions below pin an exact state
    // transition, which a prior settlement would invalidate.
    const firstNullifier = await provider.connection.getAccountInfo(nullifierPdas[0]);
    assert.isNull(
      firstNullifier,
      "batch_groth16.json is already settled on this validator (its first " +
      "nullifier marker exists). These tests require a fresh ledger. Reset it " +
      "with './scripts/dev.sh clean' and re-run, or deploy to a fresh devnet."
    );

    // Get the current state before settlement to know the pre-settlement root
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const rootBeforeBytes = Buffer.from(stateBefore.root as number[]);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    await settleViaTxData(Keypair.generate(), tx, { createdCommitments: commitmentsOf(fixture) });

    // Verify nullifier PDAs exist
    for (const pda of nullifierPdas) {
      const info = await provider.connection.getAccountInfo(pda);
      assert.ok(info, "nullifier marker PDA should exist");
      assert.ok(info!.owner.equals(program.programId), "nullifier marker PDA should be owned by PA program");
    }

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(stateAfter.nextIndex.toNumber(), nextIndexBefore + 1);

    // Verify the root changed after settlement
    const rootAfterBytes = Buffer.from(stateAfter.root as number[]);
    assert.notDeepEqual(
      rootAfterBytes,
      rootBeforeBytes,
      "Root should change after appending commitment"
    );
  });

  it("reverts on unexpected forwarder call output (ExternalCallOutputMismatch)", async () => {
    // This test uses a fixture where:
    // - timestamp = -1 (past time, forwarder will return RESULT_LT = 0x00)
    // - expected_output = 0x02 (RESULT_GT - intentionally WRONG)
    // The PA should revert with ExternalCallOutputMismatch when actual != expected.
    const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
    const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

    const mismatchNullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);

    try {
      await settleViaTxData(Keypair.generate(), mismatchTx, {
        nullifierAccounts: mismatchNullifierAccounts,
        newRootMarker: DUMMY_ROOT_MARKER,
      });
      assert.fail("expected settle to fail with ExternalCallOutputMismatch");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});

// ── Historical-root marker with a real Merkle-inclusion proof ───────────────
// Every fixture above consumes an `is_ephemeral: true` resource: the compliance
// circuit reports its consumed_commitment_tree_root as an unconstrained
// `ephemeral_root` (here, always PADDING_LEAF), never a real Merkle path. Since
// PADDING_LEAF is accepted by `is_root_valid` unconditionally, before the
// marker lookup ever runs, none of those settlements exercise the
// historical-root-marker branch.
//
// `is_ephemeral` is itself part of the resource's commitment hash, so a
// resource created as `is_ephemeral: true` (as every one above is) can never
// later be consumed through a genuine Merkle path -- flipping the flag would
// change the commitment and no longer match the leaf actually recorded
// on-chain. Proving the marker mechanism with a real inclusion proof therefore
// requires a purpose-built "committer" transaction whose created resource is
// genuinely non-ephemeral.
//
// The committer's Merkle path is baked to leaf index 1, so it must settle
// immediately after batch_groth16.json (leaf 0) and before any other
// settlement -- hence this block's position. The matching "consumer", which
// spends that leaf through the real path, runs much later (see STATE-03 part
// 2), by which point further settlements have advanced the tree and the
// committer's root is genuinely historical.
describe("protocol-adapter (STATE-03 part 1: commit a non-ephemeral leaf)", () => {
  it("settles the historical-root committer (its created resource is genuinely non-ephemeral)", async () => {
    await assertFixtureUnsettled("batch_groth16_historical_root_committer.json");

    const committerFixture = loadFixture("batch_groth16_historical_root_committer.json");
    const payload = Buffer.from(committerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(committerFixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    const nextIndexBefore = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(committerFixture),
    });
    const nextIndexAfter = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    assert.equal(nextIndexAfter, nextIndexBefore + 1);
  });
});

describe("protocol-adapter (Re-initialization guard)", () => {
  it("rejects re-initialization of PAState", async () => {
    // PAState was already initialized in the E2E before() hook.
    // A second initialize call must fail because the account already exists.
    try {
      await buildInitialize(provider.wallet.publicKey).rpc();
      assert.fail("expected re-initialization to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      // Anchor's init constraint rejects when the account already exists
      assert.match(
        haystack,
        /already in use|already been initialized|0x0/i,
        `Expected 'already in use' error, got: ${haystack}`
      );
    }
  });
});

describe("protocol-adapter (Direct settle & duplicate nullifier)", () => {
  it("rejects garbage transaction_data via settle", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    try {
      await program.methods
        .settle(Buffer.from([0, 1, 2, 3]))
        .accountsPartial({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected settle with garbage data to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  it("rejects empty transaction (zero actions) via settle", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    // The fixture's aggregated transaction with its instance's action list
    // emptied (fixture-gen's zero_action.json error variant, SEC-006
    // regression): deserializes cleanly, rejected by the PA's empty-instance
    // check.
    const emptyTx = Buffer.from(loadFixture("zero_action.json").tx_b64, "base64");

    try {
      await program.methods
        .settle(emptyTx)
        .accountsPartial({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected settle with empty transaction to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  it("rejects duplicate nullifier (double-spend) via settle_from_txdata", async () => {
    // T-06 already consumed the fixture's nullifiers. Re-uploading the same tx
    // to a fresh TxData and trying to settle must fail at nullifier creation
    // because those nullifier PDAs already exist.
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const tx = Buffer.from(fixture.tx_b64, "base64");
    const { uploadId, txData } = await uploadTxData(authority, tx);

    // The SAME nullifier PDAs (already created by T-06)
    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(allRemainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected duplicate nullifier to fail");
    } catch (e: any) {
      assertPAError(e, "DuplicateNullifier");
    }
  });
});

describe("protocol-adapter (Settle error paths)", () => {
  it("rejects wrong verifier_router_program address", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    const fakeRouter = Keypair.generate().publicKey;

    try {
      await program.methods
        .settle(Buffer.from([0, 1, 2, 3]))
        .accountsPartial({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: fakeRouter,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected wrong verifier_router_program to fail");
    } catch (e: any) {
      assertPAError(e, "VerifierRouterFailed");
    }
  });

  it("rejects insufficient remaining_accounts for nullifiers", async () => {
    // Upload the fixture but pass zero nullifier accounts.
    // The program expects 1 nullifier PDA in remaining_accounts.
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
    const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

    const { uploadId, txData } = await uploadTxData(authority, mismatchTx);

    // Pass ZERO nullifier accounts but still include forwarder+clock
    const allRemainingAccounts = buildSettleRemainingAccounts([]);

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(allRemainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected insufficient remaining_accounts to fail");
    } catch (e: any) {
      // With zero nullifier accounts, the program consumes what would be
      // the forwarder/clock slots as nullifier PDAs, then can't find the
      // forwarder in the remaining accounts. The exact error depends on
      // which check fails first.
      const code = extractPAErrorCode(e);
      const name = code !== null ? PA_ERROR_NAMES.get(code) : null;
      const validErrors = ["NullifierPdaMismatch", "UnregisteredForwarder", "InvalidTransactionData"];
      assert.isNotNull(code, "Expected a PA error code");
      assert.include(
        validErrors,
        name,
        `Expected one of ${validErrors.join("|")}, got ${name} (${code})`,
      );
    }
  });

  it("rejects unregistered forwarder program in remaining_accounts", async () => {
    // Pass correct nullifier PDAs but replace the forwarder program ID with
    // a random pubkey. The program searches external_accounts (everything
    // after the nullifier slots) for the forwarder and fails with
    // UnregisteredForwarder when it can't find it.
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
    const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

    const { uploadId, txData } = await uploadTxData(authority, mismatchTx);

    const nullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);

    // Build remaining_accounts manually with a FAKE forwarder instead of
    // the real blockTimeForwarderId
    const fakeForwarder = Keypair.generate().publicKey;
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: fakeForwarder, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(remainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected unregistered forwarder to fail");
    } catch (e: any) {
      assertPAError(e, "UnregisteredForwarder");
    }
  });
});

describe("protocol-adapter (Issue #6: Emergency Stop)", () => {
  it("stores authority on PAStateAccount after initialize", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority, "State should have authority field");
    const authorityBytes = state.authority.toBytes();
    const isZero = authorityBytes.every((b: number) => b === 0);
    assert.ok(!isZero, "Authority should not be all zeros");
  });

  it("initializes with paused=false", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(JSON.stringify(state.lifecycle), JSON.stringify({ running: {} }), "State should be Running after initialize");
  });

  it("rejects emergency_stop from non-authority", async () => {
    const nonAuthority = Keypair.generate();
    await airdrop(provider, nonAuthority, 1);

    try {
      await program.methods
        .emergencyStop()
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected emergency_stop to fail for non-authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      // Anchor's has_one constraint produces "A has one constraint was violated"
      // or our custom error "Unauthorized"
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        "Should fail with Unauthorized or has_one constraint error"
      );
    }
  });

  it("rejects propose_authority from non-authority", async () => {
    const nonAuthority = Keypair.generate();
    const newAuthority = Keypair.generate();
    await airdrop(provider, nonAuthority, 1);

    try {
      await program.methods
        .proposeAuthority(newAuthority.publicKey)
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected propose_authority to fail for non-authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        "Should fail with Unauthorized or has_one constraint error"
      );
    }
  });

  it("two-step authority transfer: propose + accept", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const currentAuthority = stateBefore.authority;

    const newAuthority = Keypair.generate();
    await airdrop(provider, newAuthority, 1);

    // Step 1: propose
    await program.methods
      .proposeAuthority(newAuthority.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Authority hasn't changed yet
    const stateAfterPropose = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      stateAfterPropose.authority.equals(currentAuthority),
      "Authority should NOT change after propose"
    );

    // Step 2: accept (signed by new authority)
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    const stateAfterAccept = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      stateAfterAccept.authority.equals(newAuthority.publicKey),
      "Authority should be updated after accept"
    );

    // Restore: propose back, accept with provider wallet
    await program.methods
      .proposeAuthority(currentAuthority)
      .accountsPartial({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();

    const stateRestored = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      stateRestored.authority.equals(currentAuthority),
      "Authority should be restored to original"
    );
  });

  it("old authority cannot call emergency_stop after transfer", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const originalAuthority = stateBefore.authority;

    const newAuthority = Keypair.generate();
    await airdrop(provider, newAuthority, 1);

    // Two-step transfer
    await program.methods
      .proposeAuthority(newAuthority.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    try {
      await program.methods
        .emergencyStop()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected emergency_stop to fail for old authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        "Should fail with Unauthorized or has_one constraint error"
      );
    }

    // Restore
    await program.methods
      .proposeAuthority(originalAuthority)
      .accountsPartial({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("proposing zero address does not brick governance", async () => {
    // Propose transfer to zero address
    await program.methods
      .proposeAuthority(PublicKey.default)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Authority is still the provider — proposal doesn't transfer
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      state.authority.equals(provider.wallet.publicKey),
      "Authority should still be provider after propose"
    );

    // Overwrite with a real candidate, complete transfer, then restore
    const realCandidate = Keypair.generate();
    await airdrop(provider, realCandidate, 1);

    await program.methods
      .proposeAuthority(realCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: realCandidate.publicKey,
      })
      .signers([realCandidate])
      .rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      stateAfter.authority.equals(realCandidate.publicKey),
      "Authority should transfer to the real candidate"
    );

    // Restore
    await program.methods
      .proposeAuthority(provider.wallet.publicKey)
      .accountsPartial({
        paState,
        authority: realCandidate.publicKey,
      })
      .signers([realCandidate])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("accept_authority fails without a pending proposal", async () => {
    const random = Keypair.generate();
    await airdrop(provider, random, 1);

    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: random.publicKey,
        })
        .signers([random])
        .rpc();
      assert.fail("accept_authority should fail with no pending proposal");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });

  it("wrong signer cannot accept a pending proposal", async () => {
    const intended = Keypair.generate();
    const attacker = Keypair.generate();
    await airdrop(provider, attacker, 1);

    // Propose the intended authority
    await program.methods
      .proposeAuthority(intended.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Attacker tries to accept
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: attacker.publicKey,
        })
        .signers([attacker])
        .rpc();
      assert.fail("attacker should not be able to accept someone else's proposal");
    } catch (e: any) {
      assertPAError(e, "Unauthorized");
    }

    // Cancel the proposal
    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("overwrite invalidates previous proposal", async () => {
    const firstCandidate = Keypair.generate();
    const secondCandidate = Keypair.generate();
    await airdrop(provider, firstCandidate, 1);
    await airdrop(provider, secondCandidate, 1);

    // Propose first candidate
    await program.methods
      .proposeAuthority(firstCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Overwrite with second candidate
    await program.methods
      .proposeAuthority(secondCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // First candidate cannot accept
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: firstCandidate.publicKey,
        })
        .signers([firstCandidate])
        .rpc();
      assert.fail("first candidate should not be able to accept after overwrite");
    } catch (e: any) {
      assertPAError(e, "Unauthorized");
    }

    // Cancel
    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("pending_authority is cleared after accept", async () => {
    const candidate = Keypair.generate();
    await airdrop(provider, candidate, 1);

    await program.methods
      .proposeAuthority(candidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: candidate.publicKey,
      })
      .signers([candidate])
      .rpc();

    // Second accept should fail — pending is cleared
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc();
      assert.fail("second accept should fail");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }

    // Restore authority
    await program.methods
      .proposeAuthority(provider.wallet.publicKey)
      .accountsPartial({
        paState,
        authority: candidate.publicKey,
      })
      .signers([candidate])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("cancel_authority_transfer clears pending proposal", async () => {
    const candidate = Keypair.generate();
    await airdrop(provider, candidate, 1);

    await program.methods
      .proposeAuthority(candidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Accept should fail — cancelled
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc();
      assert.fail("accept should fail after cancel");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });

  it("cancel_authority_transfer fails when no proposal is pending", async () => {
    try {
      await program.methods
        .cancelAuthorityTransfer()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("cancel should fail with no pending proposal");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });
});

describe("protocol-adapter (TxData Expiration)", () => {
  it("rejects txdata_init with expires_slot too soon", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 1);

    const { uploadId, uploadIdLe } = freshUploadId();

    const [txData] = PublicKey.findProgramAddressSync(
      [TX_DATA_SEED, authority.publicKey.toBuffer(), uploadIdLe],
      program.programId
    );

    const slot = await provider.connection.getSlot("confirmed");
    // Set expiry too soon (only 50 slots from now, MIN is 100)
    const expiresSlot = new anchor.BN(slot + 50);

    try {
      await program.methods
        .txdataInit(uploadId, 100, expiresSlot)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_init to fail with TxDataExpiryTooSoon");
    } catch (e: any) {
      assertPAError(e, "TxDataExpiryTooSoon");
    }
  });

  it("rejects txdata_init with expires_slot too late", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 1);

    const { uploadId, uploadIdLe } = freshUploadId();

    const [txData] = PublicKey.findProgramAddressSync(
      [TX_DATA_SEED, authority.publicKey.toBuffer(), uploadIdLe],
      program.programId
    );

    const slot = await provider.connection.getSlot("confirmed");
    // Set expiry too late (MAX + 1000 slots from now)
    const expiresSlot = new anchor.BN(slot + MAX_EXPIRY_SLOTS + 1000);

    try {
      await program.methods
        .txdataInit(uploadId, 100, expiresSlot)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_init to fail with TxDataExpiryTooLate");
    } catch (e: any) {
      assertPAError(e, "TxDataExpiryTooLate");
    }
  });

  it("accepts txdata_init with valid expires_slot", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 1);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + Math.floor((MIN_EXPIRY_SLOTS + MAX_EXPIRY_SLOTS) / 2));
    const { txData } = await initTxData(authority, 100, expiresSlot);

    const txDataAccount = await program.account.txDataAccount.fetch(txData);
    assert.equal(
      txDataAccount.expiresSlot.toNumber(),
      expiresSlot.toNumber(),
      "expires_slot should be set correctly"
    );
  });

  it("allows authority to close TxData anytime", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await initTxData(authority, 100);

    const before = await provider.connection.getAccountInfo(txData);
    assert.ok(before, "TxData should exist before close");

    const balanceBefore = await provider.connection.getBalance(authority.publicKey);

    await program.methods
      .txdataClose(uploadId)
      .accountsPartial({
        txData,
        authority: authority.publicKey,
        refund: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    const after = await provider.connection.getAccountInfo(txData);
    assert.ok(!after, "TxData should not exist after close");

    const balanceAfter = await provider.connection.getBalance(authority.publicKey);
    assert.ok(
      balanceAfter > balanceBefore,
      "Authority balance should increase after close (rent refund)"
    );
  });

  // Two-layer security model:
  // 1. PDA derivation includes authority's pubkey in seeds — attacker derives different PDA
  // 2. Anchor's seeds constraint verifies PDA matches signer — can't pass someone else's PDA

  it("rejects txdata_close for non-existent account (attacker's PDA doesn't exist)", async () => {
    // SCENARIO: Attacker derives their OWN PDA (using their pubkey in seeds).
    // Since they never created a TxData at that address, it doesn't exist.
    // This tests Anchor's account existence check.
    const authority = Keypair.generate();
    const attacker = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, attacker, 1),
    ]);

    const { uploadId, uploadIdLe, txData: authorityTxData } = await initTxData(authority, 100);

    // Attacker derives THEIR OWN PDA (different address because attacker.pubkey != authority.pubkey)
    const [attackerTxData] = PublicKey.findProgramAddressSync(
      [TX_DATA_SEED, attacker.publicKey.toBuffer(), uploadIdLe],
      program.programId
    );

    // Attacker tries to close their own (non-existent) PDA
    try {
      await program.methods
        .txdataClose(uploadId)
        .accountsPartial({
          txData: attackerTxData,
          authority: attacker.publicKey,
          refund: attacker.publicKey,
        })
        .signers([attacker])
        .rpc();
      assert.fail("should have failed - attackerTxData doesn't exist");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      // MUST be AccountNotInitialized - any other error indicates a different bug
      assert.match(
        haystack,
        /AccountNotInitialized/,
        `Expected AccountNotInitialized (account doesn't exist), got: ${haystack}`
      );
    }
  });

  it("rejects txdata_close when attacker passes authority's PDA directly (seed constraint)", async () => {
    // SCENARIO: Attacker KNOWS authority's TxData address and passes it directly.
    // But Anchor's seeds constraint computes [TX_DATA_SEED, signer.key(), upload_id].
    // Since signer is attacker, computed PDA != authority's PDA → ConstraintSeeds error.
    // This tests Anchor's PDA seed verification.
    const authority = Keypair.generate();
    const attacker = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, attacker, 1),
    ]);

    const { uploadId, uploadIdLe, txData: authorityTxData } = await initTxData(authority, 100);

    // Attacker tries to close AUTHORITY'S TxData by passing the address directly
    // Anchor will compute seeds with attacker.pubkey → different PDA → constraint fails
    try {
      await program.methods
        .txdataClose(uploadId)
        .accountsPartial({
          txData: authorityTxData,  // <-- Attacker passes authority's actual TxData
          authority: attacker.publicKey,
          refund: attacker.publicKey,
        })
        .signers([attacker])
        .rpc();
      assert.fail("should have failed - seed constraint should reject");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      // MUST be ConstraintSeeds - Anchor computes PDA from signer, doesn't match passed account
      assert.match(
        haystack,
        SEED_MISMATCH_PATTERN,
        `Expected ConstraintSeeds (PDA mismatch), got: ${haystack}`
      );
    }
  });

  it("extends TxData expiration deadline successfully", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const slot = await provider.connection.getSlot("confirmed");
    const initialExpiry = new anchor.BN(slot + 1000);
    const { uploadId, txData } = await initTxData(authority, 100, initialExpiry);

    let txDataAccount = await program.account.txDataAccount.fetch(txData);
    assert.equal(txDataAccount.expiresSlot.toNumber(), initialExpiry.toNumber());

    const currentSlot = await provider.connection.getSlot("confirmed");
    const newExpiry = new anchor.BN(currentSlot + 5000);

    await program.methods
      .txdataExtend(uploadId, newExpiry)
      .accountsStrict({
        paState,
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    txDataAccount = await program.account.txDataAccount.fetch(txData);
    assert.equal(txDataAccount.expiresSlot.toNumber(), newExpiry.toNumber(), "expires_slot should be updated");
  });

  it("rejects txdata_extend that doesn't increase expires_slot", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const slot = await provider.connection.getSlot("confirmed");
    const initialExpiry = new anchor.BN(slot + 10000);
    const { uploadId, txData } = await initTxData(authority, 100, initialExpiry);

    const currentSlot = await provider.connection.getSlot("confirmed");
    const lowerExpiry = new anchor.BN(currentSlot + 500); // Less than current expires_slot

    try {
      await program.methods
        .txdataExtend(uploadId, lowerExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_extend to fail with TxDataExtendMustIncrease");
    } catch (e: any) {
      assertPAError(e, "TxDataExtendMustIncrease");
    }
  });

  it("rejects txdata_close_expired for non-expired TxData", async () => {
    const authority = Keypair.generate();
    const cleaner = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, cleaner, 1),
    ]);

    const slot = await provider.connection.getSlot("confirmed");
    const { uploadId, txData } = await initTxData(authority, 100, new anchor.BN(slot + 50000));

    try {
      await program.methods
        .txdataCloseExpired(uploadId, authority.publicKey)
        .accountsStrict({
          txData,
          payer: cleaner.publicKey,
          refund: authority.publicKey,
        })
        .signers([cleaner])
        .rpc();
      assert.fail("expected txdata_close_expired to fail with TxDataNotExpired");
    } catch (e: any) {
      assertPAError(e, "TxDataNotExpired");
    }
  });

});

describe("protocol-adapter (TxData authority and bounds checks)", () => {

  it("rejects txdata_write that exceeds payload capacity", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await initTxData(authority, 100);

    try {
      await program.methods
        .txdataWrite(uploadId, 0, Buffer.alloc(200))
        .accountsPartial({
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_write to fail with TxDataBoundsExceeded");
    } catch (e: any) {
      assertPAError(e, "TxDataBoundsExceeded");
    }
  });

  it("rejects txdata_write from wrong authority", async () => {
    const authority = Keypair.generate();
    const wrongAuthority = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, wrongAuthority, 1),
    ]);

    const { uploadId, txData } = await initTxData(authority, 100);

    // Try to write as wrongAuthority — seed derivation uses signer's key,
    // which produces a different PDA, causing ConstraintSeeds
    try {
      await program.methods
        .txdataWrite(uploadId, 0, Buffer.alloc(10))
        .accountsPartial({
          txData,
          authority: wrongAuthority.publicKey,
        })
        .signers([wrongAuthority])
        .rpc();
      assert.fail("expected txdata_write from wrong authority to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        SEED_MISMATCH_PATTERN,
        `Expected authority constraint error, got: ${haystack}`
      );
    }
  });

  it("rejects settle_from_txdata from wrong authority", async () => {
    const authority = Keypair.generate();
    const wrongAuthority = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, wrongAuthority, 2),
    ]);

    const { uploadId, txData } = await initTxData(authority, 100);

    await program.methods
      .txdataWrite(uploadId, 0, Buffer.alloc(50))
      .accountsPartial({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: wrongAuthority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([wrongAuthority])
        .rpc();
      assert.fail("expected settle_from_txdata from wrong authority to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        SEED_MISMATCH_PATTERN,
        `Expected authority constraint error, got: ${haystack}`
      );
    }
  });

  it("rejects txdata_close with wrong refund address", async () => {
    const authority = Keypair.generate();
    const otherPubkey = Keypair.generate().publicKey;
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await initTxData(authority, 100);

    try {
      await program.methods
        .txdataClose(uploadId)
        .accountsPartial({
          txData,
          authority: authority.publicKey,
          refund: otherPubkey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_close with wrong refund to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        ADDRESS_MISMATCH_PATTERN,
        `Expected ConstraintAddress, got: ${haystack}`
      );
    }
  });

  it("rejects txdata_close_expired with wrong refund address", async () => {
    const authority = Keypair.generate();
    const cleaner = Keypair.generate();
    const wrongRefund = Keypair.generate().publicKey;
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, cleaner, 1),
    ]);

    const slot = await provider.connection.getSlot("confirmed");
    const { uploadId, txData } = await initTxData(authority, 100, new anchor.BN(slot + 50_000));

    // Hits ConstraintAddress before TxDataNotExpired
    // because Anchor validates account constraints before running the handler body
    try {
      await program.methods
        .txdataCloseExpired(uploadId, authority.publicKey)
        .accountsStrict({
          txData,
          payer: cleaner.publicKey,
          refund: wrongRefund,
        })
        .signers([cleaner])
        .rpc();
      assert.fail("expected txdata_close_expired with wrong refund to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        ADDRESS_MISMATCH_PATTERN,
        `Expected ConstraintAddress, got: ${haystack}`
      );
    }
  });
});

describe("protocol-adapter (update_expiry_config)", () => {
  it("updates expiry config successfully", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      state.minExpirySlots.toNumber(),
      50,
      "min_expiry_slots should be 50"
    );
    assert.equal(
      state.maxExpirySlots.toNumber(),
      5000,
      "max_expiry_slots should be 5000"
    );
  });

  it("rejects min >= max", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(5000), new anchor.BN(100))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with min >= max to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects min < 10", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(5), new anchor.BN(1000))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with min < 10 to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects max > SEVEN_DAYS_SLOTS", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(50), new anchor.BN(SEVEN_DAYS_SLOTS + 1))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with max > SEVEN_DAYS_SLOTS to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects wrong authority", async () => {
    const nonAuthority = Keypair.generate();
    await airdrop(provider, nonAuthority, 1);

    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected update_expiry_config from wrong authority to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        `Expected authority constraint error, got: ${haystack}`
      );
    }
  });

  it("restores default config", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(100), new anchor.BN(216_000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), 100, "min_expiry_slots should be restored to 100");
    assert.equal(state.maxExpirySlots.toNumber(), 216_000, "max_expiry_slots should be restored to 216_000");
  });
});

describe("protocol-adapter (TxData expiration enforcement)", () => {
  // On devnet, slots advance at ~2.5/s and tx confirmation takes seconds.
  // 30 slots gives enough room to init+write before expiration.
  const EXPIRY_OFFSET = 30;

  before(async () => {
    // Lower min_expiry_slots so we can create short-lived TxData
    await program.methods
      .updateExpiryConfig(new anchor.BN(10), new anchor.BN(216_000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  after(async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(100), new anchor.BN(216_000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("rejects txdata_write on expired TxData", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);
    const { uploadId, txData } = await initTxData(authority, 100, expiresSlot);

    await program.methods
      .txdataWrite(uploadId, 0, Buffer.alloc(10))
      .accountsPartial({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    try {
      await program.methods
        .txdataWrite(uploadId, 10, Buffer.alloc(10))
        .accountsPartial({
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_write on expired TxData to fail");
    } catch (e: any) {
      assertPAError(e, "TxDataExpired");
    }
  });

  it("rejects settle_from_txdata on expired TxData", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const tx = Buffer.from(fixture.tx_b64, "base64");
    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);

    const { uploadId, txData } = await uploadTxData(authority, tx, expiresSlot);

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(allRemainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected settle_from_txdata on expired TxData to fail");
    } catch (e: any) {
      assertPAError(e, "TxDataExpired");
    }
  });

  it("allows permissionless close of expired TxData", async () => {
    const authority = Keypair.generate();
    const cleaner = Keypair.generate();
    await Promise.all([
      airdrop(provider, authority, 2),
      airdrop(provider, cleaner, 1),
    ]);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + EXPIRY_OFFSET);
    const { uploadId, txData } = await initTxData(authority, 100, expiresSlot);

    const before = await provider.connection.getAccountInfo(txData);
    assert.ok(before, "TxData should exist before close");

    const refundBalanceBefore = await provider.connection.getBalance(authority.publicKey);

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    // Third-party (cleaner) calls txdata_close_expired — permissionless
    await program.methods
      .txdataCloseExpired(uploadId, authority.publicKey)
      .accountsStrict({
        txData,
        payer: cleaner.publicKey,
        refund: authority.publicKey,
      })
      .signers([cleaner])
      .rpc();

    const after = await provider.connection.getAccountInfo(txData);
    assert.ok(!after, "TxData should not exist after close_expired");

    const refundBalanceAfter = await provider.connection.getBalance(authority.publicKey);
    assert.ok(
      refundBalanceAfter > refundBalanceBefore,
      "Authority balance should increase after expired close (rent refund)"
    );
  });

  it("rejects txdata_extend with expires_slot too soon", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const { uploadId, txData, expiresSlot } = await initTxData(authority, 100, new anchor.BN(
      (await provider.connection.getSlot("confirmed")) + EXPIRY_OFFSET
    ));

    // Wait for the original expiry to pass so the extend would need to
    // satisfy bounds from current slot. Then try extending to current_slot + 5
    // which is below min_expiry_slots=10.
    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    const currentSlot = await provider.connection.getSlot("confirmed");
    const tooSoonExpiry = new anchor.BN(currentSlot + 5);

    try {
      await program.methods
        .txdataExtend(uploadId, tooSoonExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_extend with expires_slot too soon to fail");
    } catch (e: any) {
      assertPAError(e, "TxDataExpiryTooSoon");
    }
  });

  it("rejects txdata_extend with expires_slot too late", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await initTxData(authority, 100, new anchor.BN(
      (await provider.connection.getSlot("confirmed")) + 1000
    ));

    const currentSlot = await provider.connection.getSlot("confirmed");
    const tooLateExpiry = new anchor.BN(currentSlot + MAX_EXPIRY_SLOTS + 100_000);

    try {
      await program.methods
        .txdataExtend(uploadId, tooLateExpiry)
        .accountsStrict({
          paState,
          txData,
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
      assert.fail("expected txdata_extend with expires_slot too late to fail");
    } catch (e: any) {
      assertPAError(e, "TxDataExpiryTooLate");
    }
  });
});

describe("protocol-adapter (Settlement error paths — fixture variants)", () => {
  async function expectSettleError(fixtureName: string, expectedError: string) {
    const fx = loadFixture(fixtureName);
    const payload = Buffer.from(fx.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail(`expected ${expectedError} error`);
    } catch (e: any) {
      assertPAError(e, expectedError);
    }
  }

  it("rejects NonExistingRoot (wrong commitment tree root)", async () => {
    await expectSettleError("wrong_root.json", "NonExistingRoot");
  });

  it("rejects AggregationRequired (no aggregation proof)", async () => {
    await expectSettleError("no_aggregation.json", "AggregationRequired");
  });

  it("rejects InvalidProof (garbage aggregation proof bytes)", async () => {
    await expectSettleError("garbage_proof.json", "InvalidProof");
  });
});

describe("protocol-adapter (Multi-action transfer-shape settlement)", () => {
  // Successor of the imported-mainnet-transfer OOM regression: a synthetic
  // three-action transaction at least as large on the wire as the captured
  // production transfer (fixture-gen enforces the size), with event-emitted
  // payload blobs on every created resource. Settling it within the CU and
  // heap budgets is the regression being tested.

  // The mainnet wrap settlement (tx-data path, 24 static keys on the V1
  // program) measured 1,088 bytes; on the V2 program, new_root_marker and
  // the two event accounts make it 27 keys and 1,184 bytes of the
  // 1,232-byte limit. The SPL forwarder that supplies those keys is not on
  // this branch, so the in-suite multi-call settlement stands in for it:
  // both shapes grow by the same bytes when the adapter gains an account or
  // instruction data, so the suite's transaction may grow by at most the
  // mainnet margin over its baseline. Re-measure both baselines when the
  // shape changes on purpose. Lookup tables (anoma/dos-pm#60) lift the limit.
  it("the settlement transaction stays within the mainnet wrap settlement's remaining size margin", async () => {
    const TRANSACTION_SIZE_LIMIT = 1232;
    const MAINNET_WRAP_SETTLEMENT_BYTES = 1184;
    const MULTI_CALL_SETTLEMENT_BASELINE_BYTES = 633; // 15 static keys, measured on this branch

    const fx = loadFixture("batch_groth16_multi_call.json");
    const payload = Buffer.from(fx.tx_b64, "base64");
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);
    const { uploadId, txData } = await uploadTxData(authority, payload);
    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];
    const tx = await settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      Keypair.generate().publicKey,
      remainingAccounts,
    ).transaction();
    tx.feePayer = authority.publicKey;
    tx.recentBlockhash = (await provider.connection.getLatestBlockhash()).blockhash;

    const size = tx.serialize({ requireAllSignatures: false, verifySignatures: false }).length;
    const growth = size - MULTI_CALL_SETTLEMENT_BASELINE_BYTES;
    console.log(
      `multi-call settlement transaction: ${size} bytes (${tx.compileMessage().accountKeys.length} static keys), ` +
        `${growth} bytes over baseline; mainnet wrap margin ${TRANSACTION_SIZE_LIMIT - MAINNET_WRAP_SETTLEMENT_BYTES} bytes`,
    );
    assert.isAtMost(
      growth,
      TRANSACTION_SIZE_LIMIT - MAINNET_WRAP_SETTLEMENT_BYTES,
      "settlement transaction grew more than the mainnet wrap settlement's remaining margin; re-measure the mainnet shape before adding accounts or instruction data",
    );
  });

  // Adequacy guard: the original fixture existed because that transfer
  // could not settle in the default heap (the 256 KiB allocator and the
  // requestHeapFrame calls landed with it). A replacement only regression-
  // tests the OOM path if it, too, exhausts the default heap — so this must
  // FAIL without the heap frame. If it ever starts succeeding, the fixture
  // no longer stresses the heap and must grow. Runs before the successful
  // settlement so a surprise success cannot consume the nullifiers first.
  it("cannot settle the transfer-shape fixture without the extended heap budget", async () => {
    const fx = loadFixture("batch_groth16_transfer_shape.json");
    const payload = Buffer.from(fx.tx_b64, "base64");
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);
    const { uploadId, txData } = await uploadTxData(authority, payload);
    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);

    let caught: any = null;
    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .remainingAccounts(nullifierAccounts)
        .preInstructions([
          // Full CU budget but NO requestHeapFrame: only the default heap.
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([authority])
        .rpc();
    } catch (e: any) {
      caught = e;
    }
    assert.isNotNull(
      caught,
      "transfer-shape settlement succeeded in the default heap — the fixture no " +
        "longer exercises the OOM regression; increase its payload sizes",
    );

    // The failure must be genuine memory exhaustion, not a later check
    // (e.g. the dummy root marker) reached after the heap survived: a PA
    // error code would mean the program ran to a logic check, so the
    // fixture did NOT exhaust the default heap.
    const code = extractPAErrorCode(caught);
    const logs: string[] = caught?.logs ?? caught?.error?.logs ?? [];
    assert.isNull(
      code,
      `expected a runtime memory failure, got PA error code ${code} — the ` +
        "fixture settled past the heap in the default budget; increase its " +
        `payload sizes\nLogs:\n${logs.slice(-15).join("\n")}`,
    );
    assert.ok(
      logs.some((l) => /memory allocation failed|out of memory|Access violation/i.test(l)),
      `expected a memory-exhaustion log line\nLogs:\n${logs.slice(-15).join("\n")}`,
    );
  });

  it("settles the three-action transfer-shape fixture and emits payload events", async () => {
    const fx = loadFixture("batch_groth16_transfer_shape.json");
    const payload = Buffer.from(fx.tx_b64, "base64");
    assert.equal(fx.consumed_nullifiers_b64.length, 3, "fixture should have 3 actions");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
    const sig = await settleFixtureViaTxData(payload, nullifierAccounts, {
      createdCommitments: commitmentsOf(fx),
    });

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      stateAfter.nextIndex.toNumber(),
      nextIndexBefore + 3,
      "three created commitments should be appended",
    );

    await provider.connection.confirmTransaction(sig, "confirmed");
    const txResult = await provider.connection.getTransaction(sig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    assert.ok(txResult, "settlement transaction should be fetchable");
    const events = parseCpiEvents(txResult!);
    const innerCount =
      txResult!.meta?.innerInstructions?.reduce((n, g) => n + g.instructions.length, 0) ?? 0;
    console.log(
      `transfer-shape settlement: ${txResult!.meta?.computeUnitsConsumed} CU, ` +
        `${events.length} CPI events, ${innerCount} inner instructions of the ` +
        "64-instruction trace limit",
    );

    const actionEvents = events.filter((e) => e.name === "actionExecutedEvent");
    assert.equal(actionEvents.length, 3, "one actionExecutedEvent per action");
    for (const ev of actionEvents) {
      assert.equal(ev.data.actionTagCount, 2, "each action has one consumed + one created");
    }

    const txEvents = events.filter((e) => e.name === "transactionExecutedEvent");
    assert.equal(txEvents.length, 1);
    assert.equal(txEvents[0].data.tags.length, 6, "6 tags across 3 actions");
    assert.deepEqual(
      txEvents[0].data.isConsumed,
      [true, false, true, false, true, false],
      "consumed-then-created per action, in instance order",
    );

    // Each created resource carries one resource payload (512 words) and one
    // discovery payload (192 words) with deletion criterion "never", so both
    // are emitted with index 0 under the created resource's commitment tag.
    const resourceEvents = events.filter((e) => e.name === "resourcePayloadEvent");
    const discoveryEvents = events.filter((e) => e.name === "discoveryPayloadEvent");
    assert.equal(resourceEvents.length, 3, "one resource payload event per created resource");
    assert.equal(discoveryEvents.length, 3, "one discovery payload event per created resource");
    const createdTags = txEvents[0].data.tags.filter(
      (_: unknown, i: number) => !txEvents[0].data.isConsumed[i],
    );
    for (const [evs, byteLen] of [
      [resourceEvents, 2048],
      [discoveryEvents, 768],
    ] as const) {
      for (const ev of evs) {
        assert.equal(ev.data.index, 0);
        assert.equal(Buffer.from(ev.data.blob).length, byteLen);
        assert.ok(
          createdTags.some((t: number[]) => Buffer.from(t).equals(Buffer.from(ev.data.tag))),
          "payload event tag should be a created commitment",
        );
      }
    }
  });
});

describe("protocol-adapter (External call error paths)", () => {
  it("rejects settlement when forwarder CPI accounts are wrong", async () => {
    // Use the mismatch fixture (valid proof, nonce=2 nullifiers not consumed).
    // Replace SYSVAR_CLOCK_PUBKEY with a random pubkey so the CPI to btf fails.
    // The inner CPI error propagates through (btf's AccountSysvarMismatch).
    const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
    const payload = Buffer.from(mismatchFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);
    const randomAccount = Keypair.generate().publicKey;
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: randomAccount, isWritable: false, isSigner: false },
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected CPI failure");
    } catch (e: any) {
      // CPI error propagation: Solana records the INNER program's error code,
      // not the PA's remapped ExternalCallCpiFailed. The PA's From<ProgramError>
      // impl runs in Rust but the runtime has already committed the inner code.
      // btf's AccountSysvarMismatch = Anchor error 3015 (0xBC7).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.notEqual(code, PA_ERRORS["ExternalCallCpiFailed"],
        "Solana CPI error propagation: inner error code should appear, not PA's remapped code");
    }
  });

  it("rejects settlement when test-forwarder returns error", async () => {
    const failFixture = loadFixture("batch_forwarder_fail.json");
    const payload = Buffer.from(failFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(failFixture.consumed_nullifiers_b64);

    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: testForwarderId, isWritable: false, isSigner: false },
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected CPI failure from test-forwarder");
    } catch (e: any) {
      // CPI error propagation: test-forwarder's IntentionalFailure (6000)
      // propagates through instead of PA's ExternalCallCpiFailed (6019).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.equal(code, 6000,
        "test-forwarder's IntentionalFailure (6000) should propagate through CPI");
    }
  });

  it("rejects ExternalCallOutputMismatch when forwarder returns no data", async () => {
    const silentFixture = loadFixture("batch_forwarder_silent.json");
    const payload = Buffer.from(silentFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(silentFixture.consumed_nullifiers_b64);

    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: testForwarderId, isWritable: false, isSigner: false },
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected ExternalCallOutputMismatch error");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});

describe("protocol-adapter (Tree growth and multi-settlement)", () => {
  let v2TxSig: string;

  it("settles v2 fixture (next_index 1→2, depth 1→2)", async () => {
    await assertFixtureUnsettled("batch_groth16_v2.json");

    const accountInfoBefore = await provider.connection.getAccountInfo(paState);
    const sizeBefore = accountInfoBefore!.data.length;
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const v2Fixture = loadFixture("batch_groth16_v2.json");
    const payload = Buffer.from(v2Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v2Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    v2TxSig = await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(v2Fixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });

  it("verifies events from v2 settlement", async () => {
    // v2TxSig is set by the preceding test. If it is missing that settlement
    // failed, which must surface as a failure here rather than a skip: a
    // skipped test reports as pending, so a regression would cost two tests
    // and show only one red.
    assert.ok(
      v2TxSig,
      "v2 settlement did not produce a transaction signature — the preceding " +
      "'settles v2 fixture' test must have failed"
    );

    await provider.connection.confirmTransaction(v2TxSig, "confirmed");

    const txResult = await provider.connection.getTransaction(v2TxSig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    assert.ok(txResult, "v2 transaction should be fetchable");

    const events = parseCpiEvents(txResult!);

    const actionEvents = events.filter((e) => e.name === "actionExecutedEvent");
    assert.isAtLeast(actionEvents.length, 1, "Should emit actionExecutedEvent");
    assert.ok(
      Array.isArray(actionEvents[0].data.actionTreeRoot) &&
        actionEvents[0].data.actionTreeRoot.length === 32,
      "action_tree_root should be 32 bytes",
    );
    assert.equal(actionEvents[0].data.actionTagCount, 2,
      "action_tag_count should be 2 (consumed + created)");

    const txEvents = events.filter((e) => e.name === "transactionExecutedEvent");
    assert.equal(txEvents.length, 1, "Should emit exactly one transactionExecutedEvent");
    assert.equal(txEvents[0].data.tags.length, 2, "Should have 2 tags");
    assert.equal(txEvents[0].data.logicRefs.length, 2, "Should have 2 logic_refs");
    // Instance order is consumed-then-created per action; is_consumed states
    // each tag's role explicitly (indexers must not infer it from position).
    assert.deepEqual(
      txEvents[0].data.isConsumed,
      [true, false],
      "is_consumed should mark the nullifier then the commitment",
    );

    const fwdEvents = events.filter((e) => e.name === "forwarderCallExecutedEvent");
    assert.isAtLeast(fwdEvents.length, 1, "Should emit forwarderCallExecutedEvent");
    assert.ok(
      fwdEvents[0].data.forwarder.equals(blockTimeForwarderId),
      `forwarder should be ${blockTimeForwarderId}`,
    );
    const outputBytes = Buffer.from(fwdEvents[0].data.output);
    assert.deepEqual(outputBytes, Buffer.from([0x00]), "output should be RESULT_LT (0x00)");
  });

  it("retains a root marker for the v2 settlement's resulting root", async () => {
    // `newRootMarker` is a required named account, so the v2 settlement above
    // could not have succeeded without supplying it. This checks that one
    // instance, for the root the v2 settlement produced.
    const state = await program.account.paStateAccount.fetch(paState);
    const currentRoot = Buffer.from(state.root as number[]);
    const rootMarkerPda = deriveRootPda(currentRoot);

    const info = await provider.connection.getAccountInfo(rootMarkerPda);
    assert.ok(info, "Root marker should exist for the current root after settlement");
    assert.ok(
      info!.owner.equals(program.programId),
      "Root marker should be owned by the PA program",
    );
  });

  it("settles v3 fixture (next_index 2→3, depth stays 2)", async () => {
    await assertFixtureUnsettled("batch_groth16_v3.json");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const v3Fixture = loadFixture("batch_groth16_v3.json");
    const payload = Buffer.from(v3Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v3Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(v3Fixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });

  it("settles multi-call fixture with two external calls (next_index 3→4)", async () => {
    await assertFixtureUnsettled("batch_groth16_multi_call.json");

    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const nextIndexBefore = stateBefore.nextIndex.toNumber();

    const multiFixture = loadFixture("batch_groth16_multi_call.json");
    const payload = Buffer.from(multiFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(multiFixture.consumed_nullifiers_b64);

    // Two external call segments: [btf, clock, btf, clock]
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];

    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(multiFixture),
    });

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), nextIndexBefore + 1);
  });
});

// ── STATE-03 part 2: spend the committed leaf via its retained root ────────
// By now several settlements have advanced the commitment tree well past the
// root the committer produced, so that root is genuinely historical: it is
// neither the current root nor PADDING_LEAF. The only branch of
// `is_root_valid` that can still admit it is the root-marker lookup, which is
// exactly the branch no maintained test had ever exercised on a validator.
describe("protocol-adapter (STATE-03 part 2: settle against a retained historical root)", () => {
  // The consumer's root must be genuinely superseded before either of the two
  // tests below runs, otherwise `is_root_valid` would return true on its
  // *first* branch (root == current root) and the marker branch would again go
  // untested. Asserted explicitly rather than assumed from test ordering.
  function consumerHistoricalRoot(): { rootB64: string; marker: PublicKey } {
    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const historicalRoots = consumerFixture.historical_roots_b64 ?? [];
    assert.lengthOf(
      historicalRoots,
      1,
      "consumer fixture must carry exactly one historical (non-current) root",
    );
    const rootB64 = historicalRoots[0];

    // The exact trap every other fixture falls into: if the claimed root were
    // PADDING_LEAF, is_root_valid would accept it unconditionally, before the
    // marker lookup ever runs, and these tests would prove nothing about root
    // retention.
    assert.notEqual(
      rootB64,
      EMPTY_TREE_ROOT_INITIAL.toString("base64"),
      "consumer's historical root must not be PADDING_LEAF -- otherwise is_root_valid " +
        "admits it unconditionally and this test would not exercise the marker path",
    );

    return { rootB64, marker: deriveRootPda(Buffer.from(rootB64, "base64")) };
  }

  async function assertRootIsHistoricalNotCurrent(rootB64: string) {
    const state = await program.account.paStateAccount.fetch(paState);
    const currentRootB64 = Buffer.from(state.root as number[]).toString("base64");
    assert.notEqual(
      rootB64,
      currentRootB64,
      "consumer's root is still the current root -- is_root_valid would accept it on its " +
        "first branch, so the marker path would remain untested",
    );
  }

  // Runs before the success case: at this point the consumer has never
  // settled, so a rejection here is unambiguous. Its root is neither current
  // nor PADDING_LEAF, so withholding the marker leaves is_root_valid no branch
  // that can admit it. This is the half that proves the marker is load-bearing
  // rather than some other path letting the transaction through.
  it("rejects the consumer when its historical root marker is withheld (NonExistingRoot)", async () => {
    const { rootB64 } = consumerHistoricalRoot();
    await assertRootIsHistoricalNotCurrent(rootB64);

    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const payload = Buffer.from(consumerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(consumerFixture.consumed_nullifiers_b64);
    // Deliberately no additionalHistoricalRootMarkers.
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected settle to fail with NonExistingRoot");
    } catch (e: any) {
      assertPAError(e, "NonExistingRoot");
    }
  });

  it("settles the consumer when its historical root marker is supplied (retention works)", async () => {
    const { rootB64, marker } = consumerHistoricalRoot();
    await assertRootIsHistoricalNotCurrent(rootB64);

    const markerInfo = await provider.connection.getAccountInfo(marker);
    assert.ok(markerInfo, "historical root marker should exist from the committer's settlement");
    assert.ok(
      markerInfo!.owner.equals(program.programId),
      "root marker should be owned by the PA program",
    );

    await assertFixtureUnsettled("batch_groth16_historical_root.json");

    const consumerFixture = loadFixture("batch_groth16_historical_root.json");
    const payload = Buffer.from(consumerFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(consumerFixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts, {
      additionalHistoricalRootMarkers: [marker],
    });

    const nextIndexBefore = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    await settleFixtureViaTxData(payload, remainingAccounts, {
      createdCommitments: commitmentsOf(consumerFixture),
    });
    const nextIndexAfter = (await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber();
    assert.equal(nextIndexAfter, nextIndexBefore + 1, "consumer settlement should append its commitment");
  });
});


// ── Schema version guard ──────────────────────────────────────────────────
// These tests flip the version byte with dev_set_schema_version and restore
// it; they must run while the PA is Running.

describe("protocol-adapter (dev_set_schema_version tooling)", () => {
  const setSchemaVersion = (version: number) =>
    program.methods
      .devSetSchemaVersion(version)
      .accountsPartial({ paState, authority: provider.wallet.publicKey })
      .rpc();

  it("dev_set_schema_version rejects a non-authority signer", async () => {
    const intruder = Keypair.generate();
    await airdrop(provider, intruder, 1);
    const before = await program.account.paStateAccount.fetch(paState);
    try {
      await program.methods
        .devSetSchemaVersion(before.schemaVersion + 1)
        .accountsPartial({ paState, authority: intruder.publicKey })
        .signers([intruder])
        .rpc();
      assert.fail("dev_set_schema_version must require the PA authority");
    } catch (e: any) {
      assertPAError(e, "Unauthorized");
    }
    const after = await program.account.paStateAccount.fetch(paState);
    assert.equal(after.schemaVersion, before.schemaVersion, "a rejected call must not change the version");
  });

  it("dev_set_schema_version writes the byte and is reversible", async () => {
    const before = await program.account.paStateAccount.fetch(paState);
    const foreign = before.schemaVersion + 1;
    await setSchemaVersion(foreign);
    const info = await provider.connection.getAccountInfo(paState);
    assert.ok(info, "PAState account should exist");
    assert.equal(info!.data[8], foreign, "the schema version is byte 8 of the account data");

    // The instruction must accept an account of a foreign version, since that
    // is the state a migration instruction starts from.
    await setSchemaVersion(before.schemaVersion);
    const restored = await program.account.paStateAccount.fetch(paState);
    assert.equal(restored.schemaVersion, before.schemaVersion, "version restored");
  });

  describe("schema version guard", () => {
    let current: number;

    // settle_from_txdata and txdata_extend need a TxData account that already
    // exists; txdata_init is itself guarded, so these uploads are created
    // here, before the version flip below.
    let extendAuthority: Keypair;
    let extendUploadId: anchor.BN;
    let extendTxData: PublicKey;
    let settleAuthority: Keypair;
    let settleUploadId: anchor.BN;
    let settleTxData: PublicKey;
    let settleRemainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[];

    before(async () => {
      current = (await program.account.paStateAccount.fetch(paState)).schemaVersion;

      extendAuthority = Keypair.generate();
      await airdrop(provider, extendAuthority, 2);
      ({ uploadId: extendUploadId, txData: extendTxData } = await initTxData(extendAuthority, 100));

      settleAuthority = Keypair.generate();
      await airdrop(provider, settleAuthority, 2);
      const settleFixture = loadFixture("wrong_root.json");
      const settlePayload = Buffer.from(settleFixture.tx_b64, "base64");
      ({ uploadId: settleUploadId, txData: settleTxData } = await uploadTxData(settleAuthority, settlePayload));
      settleRemainingAccounts = buildSettleRemainingAccounts(
        deriveNullifierAccounts(settleFixture.consumed_nullifiers_b64)
      );

      // The suite-wide afterEach closes every registered upload after each
      // test; these two must survive across the cases, so un-register them
      // until after() hands them back.
      const ourTxDataKeys = new Set([extendTxData.toBase58(), settleTxData.toBase58()]);
      for (let i = openTxDataAccounts.length - 1; i >= 0; i--) {
        if (ourTxDataKeys.has(openTxDataAccounts[i].txData.toBase58())) {
          openTxDataAccounts.splice(i, 1);
        }
      }

      await setSchemaVersion(current + 1);
    });

    after(async () => {
      await setSchemaVersion(current);
      const restored = await program.account.paStateAccount.fetch(paState);
      assert.equal(restored.schemaVersion, current, "guard tests must leave the version as they found it");
      openTxDataAccounts.push(
        { uploadId: extendUploadId, txData: extendTxData, authority: extendAuthority },
        { uploadId: settleUploadId, txData: settleTxData, authority: settleAuthority },
      );
    });

    // Each case is an instruction that loads pa_state; with a foreign version
    // byte every one must refuse before doing anything else. emergency_stop is
    // last: it would stop the PA for every case after it.
    const cases: { name: string; run: () => Promise<unknown> }[] = [
      {
        name: "update_expiry_config",
        run: () =>
          program.methods
            .updateExpiryConfig(new anchor.BN(1), new anchor.BN(2))
            .accountsPartial({ paState, authority: provider.wallet.publicKey })
            .rpc(),
      },
      {
        name: "propose_authority",
        run: () =>
          program.methods
            .proposeAuthority(Keypair.generate().publicKey)
            .accountsPartial({ paState, authority: provider.wallet.publicKey })
            .rpc(),
      },
      {
        name: "accept_authority",
        run: () =>
          program.methods
            .acceptAuthority()
            .accountsPartial({ paState, newAuthority: provider.wallet.publicKey })
            .rpc(),
      },
      {
        name: "cancel_authority_transfer",
        run: () =>
          program.methods
            .cancelAuthorityTransfer()
            .accountsPartial({ paState, authority: provider.wallet.publicKey })
            .rpc(),
      },
      {
        name: "settle",
        run: async () => {
          // Tiny payload on purpose: the guard fires during account validation,
          // before the payload is parsed, and a real fixture exceeds the
          // transaction size limit when passed inline.
          const payload = Buffer.from([0, 1, 2, 3]);
          const payer = Keypair.generate();
          await airdrop(provider, payer, 2);
          return program.methods
            .settle(payload)
            .accountsPartial({
              paState,
              payer: payer.publicKey,
              systemProgram: SystemProgram.programId,
              newRootMarker: DUMMY_ROOT_MARKER,
              verifierRouterProgram: VERIFIER_ROUTER_ID,
              router: routerPda,
              verifierEntry: verifierEntryPda,
              verifierProgram: VERIFIER_PROGRAM_ID,
            })
            .preInstructions([
              ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
              ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
            ])
            .signers([payer])
            .rpc();
        },
      },
      {
        name: "settle_from_txdata",
        run: () =>
          program.methods
            .settleFromTxdata(settleUploadId)
            .accountsPartial({
              paState,
              txData: settleTxData,
              authority: settleAuthority.publicKey,
              systemProgram: SystemProgram.programId,
              newRootMarker: DUMMY_ROOT_MARKER,
              verifierRouterProgram: VERIFIER_ROUTER_ID,
              router: routerPda,
              verifierEntry: verifierEntryPda,
              verifierProgram: VERIFIER_PROGRAM_ID,
            })
            .remainingAccounts(settleRemainingAccounts)
            .preInstructions([
              ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
              ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
            ])
            .signers([settleAuthority])
            .rpc(),
      },
      {
        name: "txdata_init",
        run: async () => {
          const fx = loadFixture("wrong_root.json");
          const payload = Buffer.from(fx.tx_b64, "base64");
          const remaining = buildSettleRemainingAccounts(
            deriveNullifierAccounts(fx.consumed_nullifiers_b64)
          );
          return settleFixtureViaTxData(payload, remaining, { newRootMarker: DUMMY_ROOT_MARKER });
        },
      },
      {
        name: "txdata_extend",
        run: async () => {
          const laterExpiry = new anchor.BN((await provider.connection.getSlot("confirmed")) + 20_000);
          return program.methods
            .txdataExtend(extendUploadId, laterExpiry)
            .accountsStrict({ paState, txData: extendTxData, authority: extendAuthority.publicKey })
            .signers([extendAuthority])
            .rpc();
        },
      },
      {
        name: "close_markers_batch",
        run: () =>
          program.methods
            .closeMarkersBatch()
            .accountsPartial({ paState, authority: provider.wallet.publicKey })
            .remainingAccounts([])
            .rpc(),
      },
      {
        name: "emergency_stop",
        run: () =>
          program.methods
            .emergencyStop()
            .accountsPartial({ paState, authority: provider.wallet.publicKey })
            .rpc(),
      },
    ];

    for (const c of cases) {
      it(`${c.name} refuses a foreign schema version`, async () => {
        try {
          await c.run();
          assert.fail(`${c.name} must refuse an account whose schema version is not this binary's`);
        } catch (e: any) {
          assertPAError(e, "UnsupportedStateSchema");
        }
      });
    }
  });
});

// ── Close-while-running guard ────────────────────────────────────────────
// Verifies that teardown operations cannot be performed while the PA is
// running. Must run BEFORE emergency_stop pauses the protocol.

describe("protocol-adapter (close_markers_batch requires stopped state)", () => {
  it("close_markers_batch fails when PA is not stopped", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(state.lifecycle, { running: {} }, "PA should be Running at start of test");

    // Find any existing markers (settlement root markers from prior settle calls)
    const markers = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.ok(markers.length > 0, "Should have markers to close");

    try {
      await program.methods
        .closeMarkersBatch()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .remainingAccounts(markers.map(({ pubkey }) => ({
          pubkey, isWritable: true, isSigner: false,
        })))
        .rpc();
      assert.fail("close_markers_batch should fail when PA is not stopped");
    } catch (e: any) {
      assertPAError(e, "NotStopped");
    }
  });
});

// MUST BE LAST: emergency_stop permanently pauses PAState. No further
// settle operations can succeed after this block runs.
// ── SPL token forwarder through the adapter ──────────────────────────────
//
// A wrap and an unwrap settled with fixtures whose external calls target the
// SPL token forwarder. The fixtures' proofs bind the call inputs, so the
// tests rebuild the fixture's seeded user, mint and recipient and supply the
// accounts the call names. Placed after every tree-shape-dependent block
// (the historical-root fixtures assume the tree state at that point of the
// run) and before the final emergency stop.

describe("protocol-adapter (SPL token forwarder wrap and unwrap)", () => {
  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
  const [configPda] = deriveConfigPda(forwarderProgram.programId);

  function requireForwarderFixture(filename: string, flags: string): Fixture {
    try {
      return loadFixture(filename);
    } catch (e: any) {
      throw new Error(`${filename} missing (${e.message}); generate with: ${regenerateCommand(filename, flags)}`);
    }
  }
  const wrapFixture = requireForwarderFixture("spl_token_wrap.json", "--spl-token-wrap");
  const unwrapFixture = requireForwarderFixture("spl_token_unwrap.json", "--spl-token-unwrap");
  const wrap = wrapFixture.spl_token_wrap!;
  const unwrap = unwrapFixture.spl_token_unwrap!;

  // The fixture's seeded actors. The user is also the mint authority, so
  // the unwrap test can fund the escrow without depending on the wrap.
  const user = Keypair.fromSeed(Buffer.from(wrap.user_secret_key_b64, "base64"));
  const mintKeypair = Keypair.fromSeed(Buffer.from(wrap.mint_seed_b64, "base64"));
  const recipient = Keypair.fromSeed(Buffer.from(unwrap.recipient_seed_b64, "base64"));
  const mint = mintKeypair.publicKey;
  const [escrowPda] = deriveEscrowPda(forwarderProgram.programId, mint);
  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);

  const wrapAmount = BigInt(wrap.amount);
  const wrapNonce = BigInt(wrap.nonce);
  const unwrapAmount = BigInt(unwrap.amount);

  let userAta: PublicKey;
  let escrowAta: PublicKey;

  before(async () => {
    assert.ok(
      user.publicKey.equals(new PublicKey(Buffer.from(wrap.user_pubkey_b64, "base64"))),
      "seeded user must match the fixture's user"
    );
    assert.equal(mint.toBase58(), wrap.token_mint_b58, "seeded mint must match the wrap fixture's mint");
    assert.equal(mint.toBase58(), unwrap.token_mint_b58, "both fixtures must name the same mint");
    assert.equal(recipient.publicKey.toBase58(), unwrap.recipient_b58, "seeded recipient must match the fixture");

    // The suite's afterEach drains every funded keypair after each test, so
    // each test below funds the actors it uses.
    await airdrop(provider, user, 2);

    // 01-spl-token-forwarder.ts initializes the config; a --grep run of this
    // block alone initializes it here.
    if (!(await provider.connection.getAccountInfo(configPda))) {
      await forwarderProgram.methods
        .initialize(
          program.programId,
          Array.from(Buffer.from(wrap.logic_ref_b64, "base64")),
          emergencyCommittee.publicKey
        )
        .accounts({ authority: provider.wallet.publicKey })
        .rpc();
    }

    await createMint(provider.connection, user, user.publicKey, null, 6, mintKeypair);
    userAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, user.publicKey)).address;
    await mintTo(provider.connection, user, mint, userAta, user, Number(wrapAmount) * 2);
    escrowAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, escrowPda, true)).address;
  });

  /**
   * Settle `fx` from a TxData upload with the forwarder's account segment
   * after the nullifier markers. `preInstructions` are prepended so an
   * ed25519 instruction lands at index 0, where the wrap input points.
   */
  async function settleForwarderFixture(
    fx: Fixture,
    forwarderAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
    preInstructions: (authority: Keypair) => anchor.web3.TransactionInstruction[]
  ): Promise<string> {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from(fx.tx_b64, "base64"));
    const newRootMarker = await predictRootMarkerPda(commitmentsOf(fx));
    return settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      newRootMarker,
      [...deriveNullifierAccounts(fx.consumed_nullifiers_b64), ...forwarderAccounts]
    )
      .preInstructions(preInstructions(authority), true)
      .signers([authority])
      .rpc();
  }

  function forwarderSegmentHead() {
    return [
      { pubkey: forwarderProgram.programId, isWritable: false, isSigner: false },
      { pubkey: configPda, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];
  }

  /** The ed25519 instruction proving the fixture's signature over base64(sha256(wrap message)). */
  function wrapAuthorizationIx(fx: Fixture) {
    const meta = fx.spl_token_wrap!;
    const messageHash = createWrapMessageHash(
      forwarderProgram.programId,
      mint,
      BigInt(meta.amount),
      BigInt(meta.nonce),
      BigInt(meta.deadline),
      Buffer.from(meta.action_tree_root_b64, "base64")
    );
    return Ed25519Program.createInstructionWithPublicKey({
      publicKey: user.publicKey.toBytes(),
      message: Buffer.from(messageHash.toString("base64")),
      signature: Buffer.from(meta.signature_b64, "base64"),
    });
  }

  // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_pa_is_not_stopped
  it("rejects set_emergency_caller while the adapter is running", async () => {
    await airdrop(provider, emergencyCommittee, 1);
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(state.lifecycle, { running: {} }, "the adapter must be running here");

    try {
      await forwarderProgram.methods
        .setEmergencyCaller(Keypair.generate().publicKey)
        .accounts({ committee: emergencyCommittee.publicKey, paState })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("expected set_emergency_caller to fail");
    } catch (e: any) {
      assert.include(e.toString(), "ProtocolAdapterNotStopped");
    }
  });

  // Mirrors ERC20Forwarder.t.sol: test_wrap_pulls_funds_from_user
  it("settles a wrap: escrow receives the tokens and the nonce is marked used", async () => {
    await airdrop(provider, user, 1);
    await approve(provider.connection, user, userAta, escrowPda, user, Number(wrapAmount));
    const [nonceBitmapPda] = deriveNonceBitmapPda(forwarderProgram.programId, user.publicKey, wrapNonce);

    const userBefore = (await getAccount(provider.connection, userAta)).amount;
    const escrowBefore = (await getAccount(provider.connection, escrowAta)).amount;

    await settleForwarderFixture(
      wrapFixture,
      [
        ...forwarderSegmentHead(),
        { pubkey: userAta, isWritable: true, isSigner: false },
        { pubkey: escrowAta, isWritable: true, isSigner: false },
        { pubkey: escrowPda, isWritable: false, isSigner: false },
        { pubkey: nonceBitmapPda, isWritable: true, isSigner: false },
        { pubkey: TOKEN_PROGRAM_ID, isWritable: false, isSigner: false },
        { pubkey: SystemProgram.programId, isWritable: false, isSigner: false },
        { pubkey: provider.wallet.publicKey, isWritable: true, isSigner: false },
        { pubkey: mint, isWritable: false, isSigner: false },
      ],
      () => [wrapAuthorizationIx(wrapFixture)]
    );

    const userAfter = (await getAccount(provider.connection, userAta)).amount;
    const escrowAfter = (await getAccount(provider.connection, escrowAta)).amount;
    assert.equal(userAfter, userBefore - wrapAmount, "user balance decreases by the wrap amount");
    assert.equal(escrowAfter, escrowBefore + wrapAmount, "escrow holds the wrapped tokens");

    const bitmap = await provider.connection.getAccountInfo(nonceBitmapPda);
    assert.ok(bitmap, "the nonce bitmap account exists after the wrap");
    assert.ok(bitmap!.owner.equals(forwarderProgram.programId), "the forwarder owns the nonce bitmap");
    assert.ok(isNonceUsed(bitmap!.data, wrapNonce), "the wrap's nonce is marked used");
  });

  // Mirrors ERC20Forwarder.t.sol: test_unwrap_sends_funds_to_the_user
  it("settles an unwrap: the recipient receives the tokens from escrow", async () => {
    await airdrop(provider, user, 1);
    await airdrop(provider, recipient, 1);
    await mintTo(provider.connection, user, mint, escrowAta, user, Number(unwrapAmount));
    const recipientAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, recipient, mint, recipient.publicKey)
    ).address;

    const escrowBefore = (await getAccount(provider.connection, escrowAta)).amount;
    const recipientBefore = (await getAccount(provider.connection, recipientAta)).amount;

    await settleForwarderFixture(
      unwrapFixture,
      [
        ...forwarderSegmentHead(),
        { pubkey: escrowAta, isWritable: true, isSigner: false },
        { pubkey: recipientAta, isWritable: true, isSigner: false },
        { pubkey: escrowPda, isWritable: false, isSigner: false },
        { pubkey: TOKEN_PROGRAM_ID, isWritable: false, isSigner: false },
        { pubkey: mint, isWritable: false, isSigner: false },
      ],
      () => []
    );

    const escrowAfter = (await getAccount(provider.connection, escrowAta)).amount;
    const recipientAfter = (await getAccount(provider.connection, recipientAta)).amount;
    assert.equal(escrowAfter, escrowBefore - unwrapAmount, "escrow balance decreases by the unwrap amount");
    assert.equal(recipientAfter, recipientBefore + unwrapAmount, "recipient receives the unwrapped tokens");
  });
});

describe("protocol-adapter (Emergency Stop E2E — LAST)", () => {
  it("emergency_stop pauses protocol", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    assert.equal(JSON.stringify(stateBefore.lifecycle), JSON.stringify({ running: {} }), "Should be Running before emergency_stop");

    await program.methods
      .emergencyStop()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(JSON.stringify(stateAfter.lifecycle), JSON.stringify({ stopped: {} }), "Should be Stopped after emergency_stop");
  });

  it("rejects emergency_stop when already paused", async () => {
    try {
      await program.methods
        .emergencyStop()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected emergency_stop to fail when already paused");
    } catch (e: any) {
      assertPAError(e, "AlreadyStopped");
    }
  });

  it("rejects settle when paused", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    // Use a small garbage payload — the paused check fires before deserialization,
    // so any payload suffices. The full fixture is too large for a single settle instruction.
    try {
      await program.methods
        .settle(Buffer.from([0, 1, 2, 3]))
        .accountsPartial({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected settle to fail when paused");
    } catch (e: any) {
      assertPAError(e, "Stopped");
    }
  });

  it("rejects settle_from_txdata when paused", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    // Paused check fires before deserialization — minimal payload suffices
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from([0, 1, 2, 3]));

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accountsPartial({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          newRootMarker: DUMMY_ROOT_MARKER,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: VERIFIER_PROGRAM_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected settle_from_txdata to fail when paused");
    } catch (e: any) {
      assertPAError(e, "Stopped");
    }
  });
});

// ── Close instruction tests ──────────────────────────────────────────────

describe("protocol-adapter (Close instructions)", () => {
  it("close_markers_batch closes marker PDAs and refunds rent", async () => {
    // Find all 0-byte marker accounts owned by the PA program
    const allAccounts = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });

    if (allAccounts.length === 0) {
      console.log("    No markers to close (no settlements ran)");
      return;
    }

    const markersBefore = allAccounts.length;
    const balanceBefore = await provider.connection.getBalance(provider.wallet.publicKey);

    // Close in one batch (test suite creates few markers)
    const remainingAccounts = allAccounts.map(({ pubkey }) => ({
      pubkey,
      isWritable: true,
      isSigner: false,
    }));

    await program.methods
      .closeMarkersBatch()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();

    // Verify markers are gone
    const markersAfter = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.equal(markersAfter.length, 0, "All markers should be closed");

    const balanceAfter = await provider.connection.getBalance(provider.wallet.publicKey);
    assert.ok(balanceAfter > balanceBefore, "Authority should have received rent refund");
    console.log(`    Closed ${markersBefore} markers, recovered ${((balanceAfter - balanceBefore) / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
  });

  it("close_markers_batch rejects non-authority", async () => {
    const fakeAuthority = Keypair.generate();
    await airdrop(provider, fakeAuthority, 1);

    try {
      await program.methods
        .closeMarkersBatch()
        .accountsPartial({
          paState,
          authority: fakeAuthority.publicKey,
        })
        .signers([fakeAuthority])
        .rpc();
      assert.fail("Expected unauthorized close to fail");
    } catch (e: any) {
      assert.match(e.toString(), AUTHORITY_MISMATCH_PATTERN);
    }
  });

});

before(async () => {
  suiteStartBalance = await provider.connection.getBalance(provider.wallet.publicKey);
  console.log(`  [sol] suite start: wallet ${(suiteStartBalance / LAMPORTS_PER_SOL).toFixed(4)} SOL`);
});

beforeEach(async () => {
  providerBalanceBefore = await provider.connection.getBalance(provider.wallet.publicKey);
});

// After each test, close TxData accounts and drain funded keypairs back to the
// provider wallet so the same SOL circulates across the entire run.
afterEach(async () => {
  // 1. Close any open TxData accounts to recover rent to the authority keypair.
  //    The on-chain refund address is authority.publicKey (set at init time).
  for (const entry of openTxDataAccounts) {
    try {
      const info = await provider.connection.getAccountInfo(entry.txData);
      if (!info) continue; // already closed by settle or explicit close
      await program.methods
        .txdataClose(entry.uploadId)
        .accountsPartial({
          txData: entry.txData,
          authority: entry.authority.publicKey,
          refund: entry.authority.publicKey,
        })
        .signers([entry.authority])
        .rpc();
    } catch {
      // TxData may have been consumed by settle or closed by the test
    }
  }
  openTxDataAccounts.length = 0;

  // 2. Drain any remaining SOL from funded keypairs back to provider wallet.
  const { recovered, drained } = await drainKeypairs(provider, fundedKeypairs);
  fundedKeypairs.length = 0;

  const providerBalanceAfter = await provider.connection.getBalance(provider.wallet.publicKey);
  const netCost = providerBalanceBefore - providerBalanceAfter;
  console.log(
    `    [sol] spent ${(netCost / LAMPORTS_PER_SOL).toFixed(4)}, ` +
      `recovered ${(recovered / LAMPORTS_PER_SOL).toFixed(4)} ` +
      `(${drained} keypairs), ` +
      `wallet ${(providerBalanceAfter / LAMPORTS_PER_SOL).toFixed(4)} SOL`
  );
});

// End-of-suite audit: exact breakdown of where SOL went.
after(async () => {
  const suiteEndBalance = await provider.connection.getBalance(provider.wallet.publicKey);
  const totalSpent = suiteStartBalance - suiteEndBalance;

  // Query all accounts owned by the PA program
  const allAccounts = await provider.connection.getProgramAccounts(program.programId);

  // PAState is identifiable: it's the only account with data (non-zero-byte).
  // Nullifier and root markers are 0-byte accounts.
  // TxData accounts have significant data.
  let paStateRent = 0;
  let markerCount = 0;
  let markerRent = 0;
  let txdataCount = 0;
  let txdataRent = 0;

  for (const { account } of allAccounts) {
    if (account.data.length === 0) {
      // 0-byte account = nullifier or root marker
      markerCount++;
      markerRent += account.lamports;
    } else if (account.data.length > 200) {
      // Large account = TxData (unclosed)
      txdataCount++;
      txdataRent += account.lamports;
    } else {
      // PAState or other small accounts
      paStateRent += account.lamports;
    }
  }

  const accountRent = paStateRent + markerRent + txdataRent;
  const txFees = totalSpent - accountRent;

  console.log(`\n  [sol] ═══ Suite SOL Audit ═══`);
  console.log(`  [sol]   start balance:     ${(suiteStartBalance / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
  console.log(`  [sol]   end balance:       ${(suiteEndBalance / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
  console.log(`  [sol]   total spent:       ${(totalSpent / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
  console.log(`  [sol]   breakdown:`);
  console.log(`  [sol]     PAState rent:    ${(paStateRent / LAMPORTS_PER_SOL).toFixed(6)} SOL (${allAccounts.length - markerCount - txdataCount} accounts)`);
  console.log(`  [sol]     marker rent:     ${(markerRent / LAMPORTS_PER_SOL).toFixed(6)} SOL (${markerCount} nullifier/root markers × ${markerCount > 0 ? allAccounts.find(a => a.account.data.length === 0)!.account.lamports : 0} lamports each)`);
  console.log(`  [sol]     unclosed TxData: ${(txdataRent / LAMPORTS_PER_SOL).toFixed(6)} SOL (${txdataCount} accounts)`);
  console.log(`  [sol]     tx fees + dust:  ${(txFees / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
  console.log(`  [sol]   total accounted:   ${((accountRent + txFees) / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
});
