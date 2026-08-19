/**
 * Security mutation tests: submit crafted/mutated transactions to the PA
 * and verify they are rejected.
 *
 * These tests exercise error paths from the attacker's perspective.
 * Each test constructs a specific malformation and submits it via settle.
 * The assertion is that the transaction FAILS — the specific error code
 * varies depending on where the rejection occurs (PA program, Solana
 * runtime, or verifier router CPI).
 */

import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  ComputeBudgetProgram,
  SYSVAR_CLOCK_PUBKEY,
} from "@solana/web3.js";
import { assert } from "chai";
import path from "path";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";

import {
  getRouterPda,
  getVerifierEntryPda,
  VERIFIER_ROUTER_ID,
  verifierForSelector,
} from "../../scripts/verifier-utils";

import {
  PA_STATE_SEED,
  readJson,
  loadFixture,
  parseSelectorFromFixture,
  fundKeypair,
  deriveNullifierAccounts,
} from "../utils";

const IDL_PATH = path.resolve(process.cwd(), "target", "idl", "protocol_adapter.json");

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);

const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);

const fixture = loadFixture("batch_groth16.json");
const PROOF_SELECTOR = parseSelectorFromFixture(fixture.selector);
const VERIFIER_PROGRAM_ID = verifierForSelector(PROOF_SELECTOR).program;
const [routerPda] = getRouterPda(VERIFIER_ROUTER_ID);
const [verifierEntryPda] = getVerifierEntryPda(PROOF_SELECTOR, VERIFIER_ROUTER_ID);

const fundedKeypairs: Keypair[] = [];

async function airdrop(kp: Keypair, sol: number) {
  await fundKeypair(provider, kp, sol);
  fundedKeypairs.push(kp);
}

// Anchor error codes from IDL
const PA_ERRORS: Record<string, number> = Object.fromEntries(
  (readJson<{ errors?: { name: string; code: number }[] }>(IDL_PATH).errors ?? [])
    .map((e) => [e.name, e.code]),
);

const PA_ERROR_NAMES = new Map(Object.entries(PA_ERRORS).map(([k, v]) => [v, k]));

function extractPAErrorCode(e: any): number | null {
  const logs: string[] = e?.logs ?? e?.error?.logs ?? [];
  const paId = program.programId.toBase58();
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

/**
 * Submit raw transaction_data via settle. Throws on failure.
 */
async function settleRaw(
  payload: Buffer,
  remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [],
): Promise<string> {
  const payer = Keypair.generate();
  await airdrop(payer, 2);

  return program.methods
    .settle(payload)
    .accountsPartial({
      paState,
      payer: payer.publicKey,
      systemProgram: SystemProgram.programId,
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
    .signers([payer])
    .rpc();
}

/** Flip a byte at a specific offset. Returns a new buffer. */
function flipByte(buf: Buffer, offset: number): Buffer {
  const mutated = Buffer.from(buf);
  mutated[offset] ^= 0xff;
  return mutated;
}

/** Truncate a buffer. Returns a new buffer. */
function truncate(buf: Buffer, len: number): Buffer {
  return Buffer.from(buf.subarray(0, len));
}

/**
 * Assert that an async operation fails (throws). The specific error
 * doesn't matter — the test verifies the mutation is rejected.
 */
async function assertRejects(
  fn: () => Promise<any>,
  description: string,
): Promise<void> {
  try {
    await fn();
    assert.fail(`expected ${description} to be rejected`);
  } catch (e: any) {
    if (e.message?.startsWith("expected ")) throw e; // re-throw assert.fail
  }
}

// =============================================================================
// Tests
// =============================================================================

describe("Security: mutation-based settle tests", () => {
  const validTx = Buffer.from(fixture.tx_b64, "base64");
  const nullifierAccounts = deriveNullifierAccounts(
    fixture.consumed_nullifiers_b64,
    paState,
    program.programId,
  );

  // --- Empty / truncated payloads ---

  it("rejects empty payload", async () => {
    await assertRejects(
      () => settleRaw(Buffer.alloc(0)),
      "empty payload",
    );
  });

  it("rejects single-byte payload", async () => {
    await assertRejects(
      () => settleRaw(Buffer.from([0x00])),
      "single-byte payload",
    );
  });

  it("rejects truncated valid transaction", async () => {
    await assertRejects(
      () => settleRaw(truncate(validTx, 64)),
      "truncated transaction",
    );
  });

  // --- Zero-action transaction (SEC-006 regression) ---

  it("rejects zero-action transaction", async () => {
    const emptyTx = Buffer.from(loadFixture("zero_action.json").tx_b64, "base64");
    await assertRejects(
      () => settleRaw(emptyTx),
      "zero-action transaction",
    );
  });

  // --- Proof mutations ---

  it("rejects transaction with corrupted proof bytes", async () => {
    // The seal no longer sits at the end of the wire bytes (the aggregation
    // serializes proof before instance), so locate it by its bincode length
    // prefix (u64 LE 260) followed by the fixture's 4 selector bytes, then
    // corrupt pi_c[..32] (selector 4 + pi_a 64 + pi_b 128 = offset 196),
    // which both the real Groth16 check and the mock verifier's claim-digest
    // check bind. (pi_c's second half is the unused tail, which a mock seal
    // does not bind.)
    const sealLenPrefix = Buffer.alloc(8);
    sealLenPrefix.writeBigUInt64LE(260n); // Seal = selector (4) + proof (256)
    const sealMarker = Buffer.concat([sealLenPrefix, PROOF_SELECTOR]);
    const sealStart = validTx.indexOf(sealMarker);
    assert.notEqual(sealStart, -1, "seal length prefix + selector not found in tx bytes");
    const corrupted = flipByte(validTx, sealStart + 8 + 196);
    await assertRejects(
      () => settleRaw(corrupted, nullifierAccounts),
      "corrupted proof",
    );
  });

  it("rejects all-zero payload of valid length", async () => {
    await assertRejects(
      () => settleRaw(Buffer.alloc(validTx.length), nullifierAccounts),
      "all-zero payload",
    );
  });

  // --- Remaining accounts mutations ---

  it("rejects settlement with no remaining accounts", async () => {
    await assertRejects(
      () => settleRaw(validTx, []),
      "missing remaining accounts",
    );
  });

  it("rejects settlement with random remaining accounts", async () => {
    const randomAccounts = Array.from({ length: 3 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isWritable: true,
      isSigner: false,
    }));
    await assertRejects(
      () => settleRaw(validTx, randomAccounts),
      "random remaining accounts",
    );
  });
});
