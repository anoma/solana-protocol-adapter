/**
 * Security mutation tests: submit crafted/mutated transactions to an
 * initialized adapter and verify each is rejected by the check that owns
 * that malformation. Payloads that fit a transaction go inline through
 * `settle`; the full-size ones go through a TxData upload.
 */
import { AccountMeta, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { loadFixture, parseSelectorFromFixture } from "../utils/fixtures";
import { assertFails } from "../utils/helpers";
import {
  DUMMY_ROOT_MARKER,
  VERIFIER,
  buildSettleRemainingAccounts,
  deriveNullifierAccounts,
  fixture,
  program,
  settleBuilder,
  useAdapterSuite,
} from "../utils/adapterSuite";

/** BN254's base field modulus. */
const BN254_P = 21888242871839275222246405745257275088696311157297823662689037894645226208583n;

/** Whether the big-endian (x, y) of `point` is on BN254's G1, y² = x³ + 3, with both coordinates field elements. */
function isBn254G1Point(point: Buffer): boolean {
  const x = BigInt(`0x${point.subarray(0, 32).toString("hex")}`);
  const y = BigInt(`0x${point.subarray(32, 64).toString("hex")}`);
  return x < BN254_P && y < BN254_P && (y * y - (x * x * x + 3n)) % BN254_P === 0n;
}

/** Truncate a buffer. Returns a new buffer. */
function truncate(buf: Buffer, len: number): Buffer {
  return Buffer.from(buf.subarray(0, len));
}

describe("Security: mutation-based settle tests", () => {
  const { funder, settleFixtureViaTxData } = useAdapterSuite();

  const validTx = Buffer.from(fixture.tx_b64, "base64");
  const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);

  /** Submit `payload` inline via `settle`. */
  async function settleInline(payload: Buffer, remainingAccounts: AccountMeta[] = []): Promise<string> {
    const payer = await funder.fresh(2);
    return settleBuilder(payer.publicKey, payload, remainingAccounts).signers([payer]).rpc();
  }

  /** Upload `payload` to TxData and settle it: full-size payloads exceed an inline `settle`. */
  const settleUploaded = (payload: Buffer, remainingAccounts: AccountMeta[]) =>
    settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });

  // --- Empty / truncated payloads ---

  it("rejects empty payload", () =>
    assertFails(settleInline(Buffer.alloc(0)), { program, error: "InvalidTransactionData" }));

  it("rejects single-byte payload", () =>
    assertFails(settleInline(Buffer.from([0x00])), { program, error: "InvalidTransactionData" }));

  it("rejects truncated valid transaction", () =>
    assertFails(settleInline(truncate(validTx, 64)), { program, error: "InvalidTransactionData" }));

  // --- Zero-action transaction (SEC-006 regression) ---

  it("rejects zero-action transaction", async () =>
    assertFails(settleInline(Buffer.from((await loadFixture("zero_action.json")).tx_b64, "base64")), {
      program,
      error: "EmptyTransactionNotAllowed",
    }));

  // --- Proof mutations ---

  // A Groth16 seal's pi_c that is not a point of BN254's G1 is a malformed
  // proof; one that is a point, but not the proof's, fails verification. The
  // fixture's pi_c is corrupted (fixture-gen's corrupt_seal.json); the
  // generator (1, 2) is a point. Every account the settlement needs is
  // supplied, so the seal is the only defect. A verifier that reads no curve
  // points (the mock) rejects the corrupted seal as not verifying.
  it("rejects a corrupted proof as malformed off the curve and as failing verification on it", async () => {
    const corrupt = await loadFixture("corrupt_seal.json");
    const tx = Buffer.from(corrupt.tx_b64, "base64");
    const accounts = buildSettleRemainingAccounts(deriveNullifierAccounts(corrupt.consumed_nullifiers_b64));
    if (VERIFIER.malformedProof === undefined) {
      await assertFails(settleUploaded(tx, accounts), VERIFIER.rejection);
      return;
    }
    const malformedProof = VERIFIER.malformedProof;
    // The seal follows the selector: pi_a (64 bytes), pi_b (128), pi_c (64),
    // each coordinate a big-endian field element.
    const selectorAt = tx.indexOf(parseSelectorFromFixture(corrupt.selector));
    assert.equal(tx.lastIndexOf(parseSelectorFromFixture(corrupt.selector)), selectorAt, "one selector in the tx");
    const piCAt = selectorAt + 4 + 64 + 128;

    const generator = Buffer.alloc(64);
    generator[31] = 1;
    generator[63] = 2;
    const cases = [
      { name: "the fixture's pi_c", tx },
      { name: "the generator as pi_c", tx: Buffer.concat([tx.subarray(0, piCAt), generator, tx.subarray(piCAt + 64)]) },
    ];
    const onCurve = cases.map(({ tx }) => isBn254G1Point(tx.subarray(piCAt, piCAt + 64)));
    assert.deepEqual(onCurve, [false, true], "one case off the curve, one on it");

    for (const [i, { name, tx }] of cases.entries()) {
      console.log(`${name}: on the curve ${onCurve[i]}`);
      await assertFails(settleUploaded(tx, accounts), onCurve[i] ? VERIFIER.rejection : malformedProof);
    }
  });

  it("rejects all-zero payload of valid length", () =>
    assertFails(settleUploaded(Buffer.alloc(validTx.length), nullifierAccounts), {
      program,
      error: "InvalidTransactionData",
    }));

  // --- Remaining accounts mutations ---

  // The transaction consumes a resource, whose nullifier marker is missing.
  it("rejects settlement with no remaining accounts", () =>
    assertFails(settleUploaded(validTx, []), { program, error: "AccountNotEnoughKeys" }));

  // Every account is the settlement's own except the consumed resource's
  // nullifier marker, which is at another address.
  it("rejects a nullifier marker at another address", () => {
    const [marker, ...rest] = buildSettleRemainingAccounts(nullifierAccounts);
    return assertFails(settleUploaded(validTx, [{ ...marker, pubkey: Keypair.generate().publicKey }, ...rest]), {
      program,
      error: "NullifierPdaMismatch",
    });
  });
});
