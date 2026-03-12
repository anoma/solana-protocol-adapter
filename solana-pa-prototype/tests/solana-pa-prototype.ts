import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
  ComputeBudgetProgram,
  SYSVAR_CLOCK_PUBKEY,
  Transaction,
} from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import path from "path";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

import {
  getRouterPda,
  getVerifierEntryPda,
  VERIFIER_ROUTER_ID,
  GROTH16_VERIFIER_ID,
} from "../scripts/verifier-utils";

type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string; // "0x73c457ba" format
  tx_b64: string;
  tx_tampered_b64: string;
  consumed_nullifiers_b64: string[];
};

function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

function loadFixture(filename: string): Fixture {
  return readJson<Fixture>(path.resolve(process.cwd(), "tests", "fixtures", filename));
}

// Keypairs funded during tests, drained back to the provider wallet in
// afterEach() so devnet SOL circulates across the test run.
const fundedKeypairs: Keypair[] = [];

// TxData accounts created during tests, closed in afterEach() to recover rent.
const openTxDataAccounts: { uploadId: anchor.BN; txData: PublicKey; authority: Keypair }[] = [];

let providerBalanceBefore = 0;
let suiteStartBalance = 0;

async function airdrop(provider: anchor.AnchorProvider, kp: Keypair, sol: number) {
  const needed = sol * LAMPORTS_PER_SOL;
  const balance = await provider.connection.getBalance(kp.publicKey);
  if (balance >= needed) return;

  const tx = new Transaction().add(
    SystemProgram.transfer({
      fromPubkey: provider.wallet.publicKey,
      toPubkey: kp.publicKey,
      lamports: needed - balance,
    })
  );
  await provider.sendAndConfirm(tx);
  fundedKeypairs.push(kp);
}

const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex"
);

const PA_STATE_SEED = Buffer.from("pa_state");
const NULLIFIER_SEED = Buffer.from("nullifier");
const TX_DATA_SEED = Buffer.from("tx_data");
const ROOT_SEED = Buffer.from("root");

// Protocol constants matching Rust defaults (from state.rs)
const MIN_EXPIRY_SLOTS = 100;
const MAX_EXPIRY_SLOTS = 216_000;
// 7 days at 400ms/slot — matches SEVEN_DAYS_SLOTS in state.rs
const SEVEN_DAYS_SLOTS = 1_512_000;

// Anchor constraint error patterns for assertion matching
const AUTHORITY_MISMATCH_PATTERN = /Unauthorized|has.?one.*constraint.*violated|ConstraintHasOne/i;
const SEED_MISMATCH_PATTERN = /ConstraintSeeds|ConstraintHasOne|has.?one|seeds constraint|Unauthorized/i;
const ADDRESS_MISMATCH_PATTERN = /ConstraintAddress|address constraint/i;

const IDL_PATH = path.resolve(process.cwd(), "target", "idl", "solana_pa_prototype.json");

function parseSelectorFromFixture(selectorHex: string): Buffer {
  const hex = selectorHex.replace(/^0x/, "");
  if (hex.length !== 8) {
    throw new Error(`Invalid selector format: ${selectorHex} (expected 8 hex chars)`);
  }
  return Buffer.from(hex, "hex");
}

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);

const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);

const fixture = loadFixture("batch_groth16.json");

const GROTH16_SELECTOR = parseSelectorFromFixture(fixture.selector);

const [routerPda] = getRouterPda(VERIFIER_ROUTER_ID);
const [verifierEntryPda] = getVerifierEntryPda(GROTH16_SELECTOR, VERIFIER_ROUTER_ID);

// Must match `programs/block-time-forwarder/src/lib.rs::declare_id!`.
const blockTimeForwarderId = new PublicKey("3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf");

// Must match `programs/test-forwarder/src/lib.rs::declare_id!`.
const testForwarderId = new PublicKey("QfyNAtiNrw1YJAm9FzShw6oVZ4BDHojKrpje2mNNctD");

function deriveRootPda(root: Buffer): PublicKey {
  return PublicKey.findProgramAddressSync(
    [ROOT_SEED, paState.toBuffer(), root],
    program.programId
  )[0];
}

function deriveNullifierAccounts(nullifierB64s: string[]): { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] {
  return nullifierB64s.map((nfB64) => {
    const nf = Buffer.from(nfB64, "base64");
    const pubkey = PublicKey.findProgramAddressSync(
      [NULLIFIER_SEED, paState.toBuffer(), nf],
      program.programId
    )[0];
    return { pubkey, isWritable: true, isSigner: false };
  });
}

function buildSettleRemainingAccounts(
  nullifierAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
  options?: {
    additionalHistoricalRootMarkers?: PublicKey[];
    newRootMarkerPda?: PublicKey;
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
  if (options?.newRootMarkerPda) {
    accounts.push({ pubkey: options.newRootMarkerPda, isWritable: true, isSigner: false });
  }
  return accounts;
}

async function uploadTxData(
  authority: Keypair,
  payload: Buffer,
  expiresSlotOverride?: anchor.BN
): Promise<{ uploadId: anchor.BN; uploadIdLe: Buffer; txData: PublicKey }> {
  const { uploadId, uploadIdLe, txData } = await initTxData(authority, payload.length, expiresSlotOverride);

  const chunkSize = 700;
  for (let offset = 0; offset < payload.length; offset += chunkSize) {
    const chunk = payload.subarray(offset, Math.min(payload.length, offset + chunkSize));
    await program.methods
      .txdataWrite(uploadId, offset, chunk)
      .accounts({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();
  }

  return { uploadId, uploadIdLe, txData };
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
    .accounts({
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

async function createDataAccount(
  space: number,
  owner: PublicKey = testForwarderId,
): Promise<Keypair> {
  const funder = Keypair.generate();
  await airdrop(provider, funder, 2);
  const account = Keypair.generate();
  const lamports = await provider.connection.getMinimumBalanceForRentExemption(space);
  const tx = new anchor.web3.Transaction().add(
    SystemProgram.createAccount({
      fromPubkey: funder.publicKey,
      newAccountPubkey: account.publicKey,
      space,
      lamports,
      programId: owner,
    }),
  );
  await provider.sendAndConfirm(tx, [funder, account]);
  return account;
}

function parseAnchorEvents(logs: string[]) {
  const coder = new anchor.BorshCoder(program.idl);
  const parser = new anchor.EventParser(program.programId, coder);
  return [...parser.parseLogs(logs)];
}

async function settleFixtureViaTxData(
  payload: Buffer,
  remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[],
): Promise<string> {
  const authority = Keypair.generate();
  await airdrop(provider, authority, 2);
  const { uploadId, txData } = await uploadTxData(authority, payload);
  return program.methods
    .settleFromTxdata(uploadId)
    .accounts({
      paState,
      txData,
      authority: authority.publicKey,
      systemProgram: SystemProgram.programId,
      verifierRouterProgram: VERIFIER_ROUTER_ID,
      router: routerPda,
      verifierEntry: verifierEntryPda,
      verifierProgram: GROTH16_VERIFIER_ID,
    })
    .remainingAccounts(remainingAccounts)
    .preInstructions([
      ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
      ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
    ])
    .signers([authority])
    .rpc();
}

describe("solana-pa-prototype (Groth16 batch aggregation E2E)", () => {
  const tx = Buffer.from(fixture.tx_b64, "base64");
  const txTampered = Buffer.from(fixture.tx_tampered_b64, "base64");

  const remainingAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
  const nullifierPdas = remainingAccounts.map((a) => a.pubkey);

  async function settleViaTxData(
    authority: Keypair,
    payload: Buffer,
    options?: {
      nullifierAccounts?: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[];
      newRootMarkerPda?: PublicKey;
      additionalHistoricalRootMarkers?: PublicKey[];
    }
  ) {
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await uploadTxData(authority, payload);

    const allRemainingAccounts = buildSettleRemainingAccounts(
      options?.nullifierAccounts ?? remainingAccounts,
      options
    );

    return program.methods
      .settleFromTxdata(uploadId)
      .accounts({
        paState,
        txData,
        authority: authority.publicKey,
        systemProgram: SystemProgram.programId,
        verifierRouterProgram: VERIFIER_ROUTER_ID,
        router: routerPda,
        verifierEntry: verifierEntryPda,
        verifierProgram: GROTH16_VERIFIER_ID,
      })
      .remainingAccounts(allRemainingAccounts)
      .preInstructions([
        ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
      ])
      .signers([authority])
      .rpc();
  }

  let genesisRootMarkerPda: PublicKey;

  before(async () => {
    genesisRootMarkerPda = deriveRootPda(EMPTY_TREE_ROOT_INITIAL);

    try {
      await program.account.paStateAccount.fetch(paState);
    } catch {
      // Initialize with genesis root marker in remaining_accounts
      await program.methods
        .initialize()
        .accounts({
          paState,
          payer: provider.wallet.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .remainingAccounts([
          { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
        ])
        .rpc();
    }
  });

  it("creates genesis root marker on initialize", async () => {
    const info = await provider.connection.getAccountInfo(genesisRootMarkerPda);
    assert.ok(info, "Genesis root marker PDA should exist after initialize");
    assert.ok(
      info!.owner.equals(program.programId),
      "Genesis root marker should be owned by PA program"
    );
    assert.equal(info!.data.length, 0, "Root marker should be 0 bytes (existence-only)");
  });

  it("initializes with depth 1 (variable-depth tree)", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      state.currentDepth,
      1,
      "Initial tree depth should be 1 (capacity = 2 leaves)"
    );
    assert.equal(
      state.frontier.length,
      1,
      "Initial frontier should have 1 element (depth 1)"
    );
    const rootBytes = Buffer.from(state.root as number[]);
    assert.deepEqual(
      rootBytes,
      EMPTY_TREE_ROOT_INITIAL,
      "Initial root should be ZEROS[0] for depth-1 tree"
    );
  });

  it("account size matches expected size for current depth (no over-allocation)", async () => {
    // Space formula: BASE_SPACE (99) + VEC_OVERHEAD (4) + 32 * depth
    const BASE_SPACE = 99;
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
    // Clone the valid tx bytes and patch Delta::Proof to Delta::Witness
    const txWitness = Buffer.from(tx);

    // In bincode, Delta enum is serialized as:
    // - u32 variant index (0 = Witness, 1 = Proof)
    // - u64 length prefix (DeltaProof uses serialize_bytes → 65 = 0x41)
    // - payload bytes (DeltaProof = 65 bytes)
    //
    // Patching variant from 1 (Proof) to 0 (Witness) causes the deserializer
    // to misinterpret subsequent bytes, shifting all fields. This will either
    // produce an ExpectedDeltaProof error or an Invalid transaction data error.
    // Search for the unique 12-byte pattern: variant(1) + length(65) to avoid
    // matching other [01 00 00 00] occurrences (e.g. Vec length at offset 0).
    const deltaProofHeader = Buffer.from([
      0x01, 0x00, 0x00, 0x00, // variant index 1 (Proof)
      0x41, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // length prefix 65
    ]);

    const idx = txWitness.indexOf(deltaProofHeader);
    if (idx === -1) {
      throw new Error("Could not find Delta::Proof variant+length tag in tx bytes");
    }

    // Patch variant from 1 (Proof) to 0 (Witness)
    txWitness.writeUInt32LE(0, idx);

    try {
      await settleViaTxData(Keypair.generate(), txWitness);
      assert.fail("expected settle to fail");
    } catch (e: any) {
      // Patching the Delta variant from Proof→Witness corrupts the Borsh layout.
      // Depending on how the remaining bytes are interpreted, the PA may fail with
      // ExpectedDeltaProof (reaches the delta check) or InvalidTransactionData
      // (Borsh deserialization fails first). Both are correct rejections.
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.include(
        [PA_ERRORS["ExpectedDeltaProof"], PA_ERRORS["InvalidTransactionData"]],
        code!,
        `Expected ExpectedDeltaProof (${PA_ERRORS["ExpectedDeltaProof"]}) or ` +
          `InvalidTransactionData (${PA_ERRORS["InvalidTransactionData"]}), got ${code}`,
      );
    }
  });

  it("rejects a tampered tx (proof binding)", async () => {
    try {
      await settleViaTxData(Keypair.generate(), txTampered);
      assert.fail("expected settle to fail");
    } catch (e: any) {
      // The PA calls the verifier router via CPI, which calls the groth16 verifier.
      // The groth16 verifier rejects the tampered proof with VerificationError (6000).
      // Solana's CPI error propagation records the INNER program's error code in the
      // PA's failure line — so we see 0x1770 (6000) instead of the PA's VerifierRouterFailed (6013).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      // The inner verifier's error code 6000 propagates through CPI
      assert.equal(
        code,
        6000,
        "groth16 verifier's VerificationError (6000) should propagate through CPI",
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

    // Get the current state before settlement to know the pre-settlement root
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const rootBeforeBytes = Buffer.from(stateBefore.root as number[]);

    await settleViaTxData(Keypair.generate(), tx);

    // Verify nullifier PDAs exist
    for (const pda of nullifierPdas) {
      const info = await provider.connection.getAccountInfo(pda);
      assert.ok(info, "nullifier marker PDA should exist");
      assert.ok(info!.owner.equals(program.programId), "nullifier marker PDA should be owned by PA program");
    }

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(stateAfter.nextIndex.toNumber(), 1);

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
      });
      assert.fail("expected settle to fail with ExternalCallOutputMismatch");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});

describe("solana-pa-prototype (Re-initialization guard)", () => {
  it("rejects re-initialization of PAState", async () => {
    // PAState was already initialized in the E2E before() hook.
    // A second initialize call must fail because the account already exists.
    const genesisRootMarkerPda = deriveRootPda(EMPTY_TREE_ROOT_INITIAL);

    try {
      await program.methods
        .initialize()
        .accounts({
          paState,
          payer: provider.wallet.publicKey,
          systemProgram: SystemProgram.programId,
        })
        .remainingAccounts([
          { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
        ])
        .rpc();
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

describe("solana-pa-prototype (Direct settle & duplicate nullifier)", () => {
  it("rejects garbage transaction_data via settle", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    try {
      await program.methods
        .settle(Buffer.from([0, 1, 2, 3]))
        .accounts({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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

describe("solana-pa-prototype (Settle error paths)", () => {
  it("rejects wrong verifier_router_program address", async () => {
    const payer = Keypair.generate();
    await airdrop(provider, payer, 2);

    const fakeRouter = Keypair.generate().publicKey;

    try {
      await program.methods
        .settle(Buffer.from([0, 1, 2, 3]))
        .accounts({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: fakeRouter,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected wrong verifier_router_program to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        ADDRESS_MISMATCH_PATTERN,
        `Expected ConstraintAddress, got: ${haystack}`
      );
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
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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

describe("solana-pa-prototype (Issue #6: Emergency Stop)", () => {
  it("stores authority on PAStateAccount after initialize", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority, "State should have authority field");
    const authorityBytes = state.authority.toBytes();
    const isZero = authorityBytes.every((b: number) => b === 0);
    assert.ok(!isZero, "Authority should not be all zeros");
  });

  it("initializes with paused=false", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.paused, false, "State should be unpaused after initialize");
  });

  it("rejects emergency_stop from non-authority", async () => {
    const nonAuthority = Keypair.generate();
    await airdrop(provider, nonAuthority, 1);

    try {
      await program.methods
        .emergencyStop()
        .accounts({
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

  it("rejects transfer_authority from non-authority", async () => {
    const nonAuthority = Keypair.generate();
    const newAuthority = Keypair.generate();
    await airdrop(provider, nonAuthority, 1);

    try {
      await program.methods
        .transferAuthority(newAuthority.publicKey)
        .accounts({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected transfer_authority to fail for non-authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        "Should fail with Unauthorized or has_one constraint error"
      );
    }
  });

  it("transfers authority when called by current authority", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const currentAuthority = stateBefore.authority;

    const newAuthority = Keypair.generate();
    await airdrop(provider, newAuthority, 1);

    await program.methods
      .transferAuthority(newAuthority.publicKey)
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.ok(
      stateAfter.authority.equals(newAuthority.publicKey),
      "Authority should be updated to new authority"
    );

    await program.methods
      .transferAuthority(currentAuthority)
      .accounts({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
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

    await program.methods
      .transferAuthority(newAuthority.publicKey)
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    try {
      await program.methods
        .emergencyStop()
        .accounts({
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

    await program.methods
      .transferAuthority(originalAuthority)
      .accounts({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();
  });

  // NOTE: The following tests are destructive - they pause the protocol.
  // We use a separate describe block with its own PAState to avoid affecting other tests.
});

describe("solana-pa-prototype (TxData Expiration)", () => {
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
        .accounts({
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
        .accounts({
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
      .accounts({
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
        .accounts({
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
        .accounts({
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

describe("solana-pa-prototype (TxData authority and bounds checks)", () => {

  it("rejects txdata_write that exceeds payload capacity", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const { uploadId, txData } = await initTxData(authority, 100);

    try {
      await program.methods
        .txdataWrite(uploadId, 0, Buffer.alloc(200))
        .accounts({
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
        .accounts({
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
      .accounts({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accounts({
          paState,
          txData,
          authority: wrongAuthority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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
        .accounts({
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

describe("solana-pa-prototype (update_expiry_config)", () => {
  it("updates expiry config successfully", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
      .accounts({
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
        .accounts({
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
        .accounts({
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
        .accounts({
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
        .accounts({
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
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), 100, "min_expiry_slots should be restored to 100");
    assert.equal(state.maxExpirySlots.toNumber(), 216_000, "max_expiry_slots should be restored to 216_000");
  });
});

describe("solana-pa-prototype (TxData expiration enforcement)", () => {
  before(async () => {
    // Lower min_expiry_slots to 10 so we can create short-lived TxData
    await program.methods
      .updateExpiryConfig(new anchor.BN(10), new anchor.BN(216_000))
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  after(async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(100), new anchor.BN(216_000))
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("rejects txdata_write on expired TxData", async () => {
    const authority = Keypair.generate();
    await airdrop(provider, authority, 2);

    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + 12);
    const { uploadId, txData } = await initTxData(authority, 100, expiresSlot);

    await program.methods
      .txdataWrite(uploadId, 0, Buffer.alloc(10))
      .accounts({
        txData,
        authority: authority.publicKey,
      })
      .signers([authority])
      .rpc();

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    try {
      await program.methods
        .txdataWrite(uploadId, 10, Buffer.alloc(10))
        .accounts({
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
    const expiresSlot = new anchor.BN(slot + 12);

    const { uploadId, txData } = await uploadTxData(authority, tx, expiresSlot);

    await waitForSlotPast(provider.connection, expiresSlot.toNumber());

    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await program.methods
        .settleFromTxdata(uploadId)
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
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
    const expiresSlot = new anchor.BN(slot + 12);
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
      (await provider.connection.getSlot("confirmed")) + 12
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

describe("solana-pa-prototype (Settlement error paths — fixture variants)", () => {
  async function expectSettleError(fixtureName: string, expectedError: string) {
    const fx = loadFixture(fixtureName);
    const payload = Buffer.from(fx.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    try {
      await settleFixtureViaTxData(payload, remainingAccounts);
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

describe("solana-pa-prototype (External call error paths)", () => {
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
      await settleFixtureViaTxData(payload, remainingAccounts);
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
      await settleFixtureViaTxData(payload, remainingAccounts);
      assert.fail("expected CPI failure from test-forwarder");
    } catch (e: any) {
      // CPI error propagation: test-forwarder's IntentionalFailure (6000)
      // propagates through instead of PA's ExternalCallCpiFailed (6018).
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
      await settleFixtureViaTxData(payload, remainingAccounts);
      assert.fail("expected ExternalCallOutputMismatch error");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});

describe("solana-pa-prototype (Tree growth and multi-settlement)", () => {
  let v2TxSig: string;

  it("settles v2 fixture (next_index 1→2, depth 1→2)", async () => {
    const accountInfoBefore = await provider.connection.getAccountInfo(paState);
    const sizeBefore = accountInfoBefore!.data.length;

    const v2Fixture = loadFixture("batch_groth16_v2.json");
    const payload = Buffer.from(v2Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v2Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    v2TxSig = await settleFixtureViaTxData(payload, remainingAccounts);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), 2, "next_index should be 2 after v2 settlement");
    // Expand-after-fill: next_index (2) == capacity (2^1), so depth grows 1→2
    assert.equal(state.currentDepth, 2, "depth should grow to 2 (expand-after-fill at capacity)");

    const accountInfoAfter = await provider.connection.getAccountInfo(paState);
    assert.ok(
      accountInfoAfter!.data.length > sizeBefore,
      `Account should grow from depth 1 to 2 (${sizeBefore} → ${accountInfoAfter!.data.length})`
    );
  });

  it("verifies events from v2 settlement", async () => {
    // Wait for the transaction to be fully indexed by the validator
    await provider.connection.confirmTransaction(v2TxSig, "confirmed");

    const txResult = await provider.connection.getTransaction(v2TxSig, {
      commitment: "confirmed",
      maxSupportedTransactionVersion: 0,
    });
    assert.ok(txResult, "v2 transaction should be fetchable");

    const logs = txResult!.meta?.logMessages ?? [];

    const events = parseAnchorEvents(logs);

    // Anchor SDK converts event names to camelCase
    // ActionExecutedEvent → actionExecutedEvent
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

    const fwdEvents = events.filter((e) => e.name === "forwarderCallExecutedEvent");
    assert.isAtLeast(fwdEvents.length, 1, "Should emit forwarderCallExecutedEvent");
    assert.ok(
      fwdEvents[0].data.forwarder.equals(blockTimeForwarderId),
      `forwarder should be ${blockTimeForwarderId}`,
    );
    const outputBytes = Buffer.from(fwdEvents[0].data.output);
    assert.deepEqual(outputBytes, Buffer.from([0x00]), "output should be RESULT_LT (0x00)");
  });

  it("does not create root marker when PDA not passed", async () => {
    // v2 settlement did not pass a newRootMarkerPda. Verify the PA's current
    // root does NOT have a root marker — the PA only creates markers when the
    // correct PDA is provided as the last remaining_account.
    const state = await program.account.paStateAccount.fetch(paState);
    const currentRoot = Buffer.from(state.root as number[]);
    const rootMarkerPda = deriveRootPda(currentRoot);

    const info = await provider.connection.getAccountInfo(rootMarkerPda);
    assert.isNull(
      info,
      "Root marker should NOT exist when PDA was not passed in remaining_accounts",
    );
  });

  it("settles v3 fixture (next_index 2→3, depth stays 2)", async () => {
    const v3Fixture = loadFixture("batch_groth16_v3.json");
    const payload = Buffer.from(v3Fixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(v3Fixture.consumed_nullifiers_b64);
    const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await settleFixtureViaTxData(payload, remainingAccounts);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), 3, "next_index should be 3 after v3 settlement");
    assert.equal(state.currentDepth, 2, "depth should stay at 2 (capacity 4, only 3 used)");
  });

  it("settles multi-call fixture with two external calls (next_index 3→4)", async () => {
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

    await settleFixtureViaTxData(payload, remainingAccounts);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), 4, "next_index should be 4 after multi-call settlement");
  });
});

describe("solana-pa-prototype (OutputAccount mode)", () => {
  it("settles forwarder-output fixture via OutputAccount mode (next_index 4→5)", async () => {
    const outputFixture = loadFixture("batch_forwarder_output.json");
    const payload = Buffer.from(outputFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(outputFixture.consumed_nullifiers_b64);

    // Create a data account owned by test-forwarder for writing output
    const dataAccount = await createDataAccount(10);

    // remaining_accounts: [nullifier_pda, test_forwarder, writable_data_account]
    // Fixture uses OutputAccount { index: 2, offset: 0, len: 4 }
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: testForwarderId, isWritable: false, isSigner: false },
      { pubkey: dataAccount.publicKey, isWritable: true, isSigner: false },
    ];

    await settleFixtureViaTxData(payload, remainingAccounts);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.nextIndex.toNumber(), 5, "next_index should be 5 after output-account settlement");

    // Verify the data account was written by the forwarder
    const dataAccountInfo = await provider.connection.getAccountInfo(dataAccount.publicKey);
    assert.ok(dataAccountInfo, "Data account should still exist");
    const writtenBytes = dataAccountInfo!.data.subarray(0, 4);
    assert.deepEqual(
      writtenBytes,
      Buffer.from([0x01, 0x02, 0x03, 0x04]),
      "Forwarder should have written expected bytes to data account"
    );
  });
});

describe("solana-pa-prototype (OutputAccount error paths)", () => {
  // The forwarder-output fixture uses OutputAccount { index: 2, offset: 0, len: 4 }
  // and expected_output [0x01, 0x02, 0x03, 0x04]. These tests manipulate remaining_accounts
  // to trigger each error path in read_forwarder_output (cpi.rs:89-103).

  const outputFixture = loadFixture("batch_forwarder_output.json");

  it("rejects OutputAccount when index is out of bounds", async () => {
    const payload = Buffer.from(outputFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(outputFixture.consumed_nullifiers_b64);

    // Fixture expects remaining_accounts[2] (index=2), but we only provide
    // [nullifier_pda, test_forwarder] — index 2 does not exist.
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: testForwarderId, isWritable: false, isSigner: false },
      // No data account at index 2
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts);
      assert.fail("expected ExternalCallOutputMismatch (index OOB)");
    } catch (e: any) {
      // CPI to test-forwarder fails because it needs a writable account in
      // remaining_accounts[0] (MODE_WRITE_ACCOUNT) but no account is passed.
      // The CPI failure propagates the inner error code through.
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
    }
  });

  it("rejects OutputAccount when data is shorter than offset+len", async () => {
    const payload = Buffer.from(outputFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(outputFixture.consumed_nullifiers_b64);

    // Create a data account with only 2 bytes — fixture expects len=4.
    // The forwarder writes min(payload.len(), data.len()) = 2 bytes.
    // Then PA reads remaining_accounts[2] with offset=0, len=4 → end=4 > 2 → error.
    const dataAccount = await createDataAccount(2);

    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: testForwarderId, isWritable: false, isSigner: false },
      { pubkey: dataAccount.publicKey, isWritable: true, isSigner: false },
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts);
      assert.fail("expected ExternalCallOutputMismatch (data too short)");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});

// MUST BE LAST: emergency_stop permanently pauses PAState. No further
// settle operations can succeed after this block runs.
describe("solana-pa-prototype (Emergency Stop E2E — LAST)", () => {
  it("emergency_stop pauses protocol", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    assert.equal(stateBefore.paused, false, "Should be unpaused before emergency_stop");

    await program.methods
      .emergencyStop()
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(stateAfter.paused, true, "Should be paused after emergency_stop");
  });

  it("rejects emergency_stop when already paused", async () => {
    try {
      await program.methods
        .emergencyStop()
        .accounts({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected emergency_stop to fail when already paused");
    } catch (e: any) {
      assertPAError(e, "AlreadyPaused");
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
        .accounts({
          paState,
          payer: payer.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([payer])
        .rpc();
      assert.fail("expected settle to fail when paused");
    } catch (e: any) {
      assertPAError(e, "Paused");
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
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: routerPda,
          verifierEntry: verifierEntryPda,
          verifierProgram: GROTH16_VERIFIER_ID,
        })
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
        ])
        .signers([authority])
        .rpc();
      assert.fail("expected settle_from_txdata to fail when paused");
    } catch (e: any) {
      assertPAError(e, "Paused");
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
        .accounts({
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
  //    Uses sendRawTransaction directly — provider.sendAndConfirm fails because
  //    Anchor's provider tries to co-sign with the wallet, which isn't needed here.
  const MIN_DRAIN = 5000;
  let recovered = 0;
  let drained = 0;
  for (const kp of fundedKeypairs) {
    try {
      const balance = await provider.connection.getBalance(kp.publicKey);
      if (balance <= MIN_DRAIN) continue;
      const drainAmount = balance - MIN_DRAIN;
      const drainTx = new Transaction().add(
        SystemProgram.transfer({
          fromPubkey: kp.publicKey,
          toPubkey: provider.wallet.publicKey,
          lamports: drainAmount,
        })
      );
      drainTx.recentBlockhash = (await provider.connection.getLatestBlockhash()).blockhash;
      drainTx.feePayer = kp.publicKey;
      drainTx.sign(kp);
      const sig = await provider.connection.sendRawTransaction(drainTx.serialize());
      await provider.connection.confirmTransaction(sig);
      recovered += drainAmount;
      drained++;
    } catch {
      // Best-effort — tx may fail if keypair was already drained
    }
  }
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
