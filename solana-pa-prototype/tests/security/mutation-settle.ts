/**
 * Security mutation tests: submit crafted/mutated transactions to the PA
 * and verify they are rejected with the correct error.
 *
 * These tests exercise error paths from the attacker's perspective.
 * Each test constructs a specific malformation and submits it via settle.
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
import { SolanaPaPrototype } from "../../target/types/solana_pa_prototype";

import {
  getRouterPda,
  getVerifierEntryPda,
  VERIFIER_ROUTER_ID,
  GROTH16_VERIFIER_ID,
} from "../../scripts/verifier-utils";

import {
  PA_STATE_SEED,
  readJson,
  loadFixture,
  parseSelectorFromFixture,
  fundKeypair,
  deriveNullifierAccounts,
} from "../utils";

const IDL_PATH = path.resolve(process.cwd(), "target", "idl", "solana_pa_prototype.json");

const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);

const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;
const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);

const fixture = loadFixture("batch_groth16.json");
const GROTH16_SELECTOR = parseSelectorFromFixture(fixture.selector);
const [routerPda] = getRouterPda(VERIFIER_ROUTER_ID);
const [verifierEntryPda] = getVerifierEntryPda(GROTH16_SELECTOR, VERIFIER_ROUTER_ID);

// Track funded keypairs for cleanup
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
 * Submit a raw transaction_data buffer via settle. Returns the tx signature
 * on success, or throws on failure (which is what we expect for mutations).
 */
async function settleRaw(
  payload: Buffer,
  remainingAccounts: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[] = [],
): Promise<string> {
  const payer = Keypair.generate();
  await airdrop(payer, 2);

  return program.methods
    .settle(payload)
    .accounts({
      paState,
      payer: payer.publicKey,
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
    .signers([payer])
    .rpc();
}

// --- Byte-level mutation helpers ---

/**
 * Flip a byte at a specific offset in a buffer. Returns a new buffer.
 */
function flipByte(buf: Buffer, offset: number): Buffer {
  const mutated = Buffer.from(buf);
  mutated[offset] ^= 0xff;
  return mutated;
}

/**
 * Zero out a range of bytes. Returns a new buffer.
 */
function zeroRange(buf: Buffer, start: number, len: number): Buffer {
  const mutated = Buffer.from(buf);
  mutated.fill(0, start, start + len);
  return mutated;
}

/**
 * Truncate a buffer to the given length. Returns a new buffer.
 */
function truncate(buf: Buffer, len: number): Buffer {
  return Buffer.from(buf.subarray(0, len));
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
    try {
      await settleRaw(Buffer.alloc(0));
      assert.fail("expected empty payload to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  it("rejects single-byte payload", async () => {
    try {
      await settleRaw(Buffer.from([0x00]));
      assert.fail("expected single-byte payload to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  it("rejects truncated valid transaction", async () => {
    try {
      await settleRaw(truncate(validTx, 64));
      assert.fail("expected truncated tx to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  // --- Zero-action transaction (SEC-006 regression) ---

  it("rejects zero-action transaction", async () => {
    // Bincode-serialized Transaction with zero actions, dummy delta/aggregation
    const emptyTx = Buffer.from(
      "000000000000000001000000410000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001400000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
      "hex",
    );
    try {
      await settleRaw(emptyTx);
      assert.fail("expected empty tx to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  // --- Proof mutations ---

  it("rejects transaction with corrupted aggregation proof bytes", async () => {
    // Flip a byte in the aggregation proof region of the serialized tx.
    // The exact offset depends on bincode layout; flipping near the end
    // targets the proof bytes.
    const corrupted = flipByte(validTx, validTx.length - 10);
    try {
      await settleRaw(corrupted, nullifierAccounts);
      assert.fail("expected corrupted proof to fail");
    } catch (e: any) {
      // Could be InvalidTransactionData (deser fails) or VerifierRouterFailed (proof invalid)
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "should produce a PA error");
    }
  });

  it("rejects transaction with all-zero payload of valid length", async () => {
    const zeros = Buffer.alloc(validTx.length);
    try {
      await settleRaw(zeros, nullifierAccounts);
      assert.fail("expected all-zero payload to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidTransactionData");
    }
  });

  // --- Remaining accounts mutations ---

  it("rejects settlement with no remaining accounts", async () => {
    try {
      await settleRaw(validTx, []);
      assert.fail("expected missing remaining accounts to fail");
    } catch (e: any) {
      // Should fail at root validation, nullifier creation, or external calls
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "should produce a PA error");
    }
  });

  it("rejects settlement with random remaining accounts", async () => {
    const randomAccounts = Array.from({ length: 3 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isWritable: true,
      isSigner: false,
    }));
    try {
      await settleRaw(validTx, randomAccounts);
      assert.fail("expected random remaining accounts to fail");
    } catch (e: any) {
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "should produce a PA error");
    }
  });
});
