/**
 * Security mutation tests: submit crafted/mutated transactions to an
 * initialized adapter and verify each is rejected by the check that owns
 * that malformation. Payloads that fit a transaction go inline through
 * `settle`; the full-size ones go through a TxData upload.
 */
import { AccountMeta, Keypair } from "@solana/web3.js";
import { loadFixture } from "../utils/fixtures";
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
      error: "InvalidTransactionData",
    }));

  // --- Proof mutations ---

  // The primary fixture with its seal's pi_c[0] flipped and the selector
  // intact (fixture-gen's corrupt_seal.json variant): the router routes it
  // to the fixture's verifier, which rejects the malformed point.
  // Every account the settlement needs is supplied, so the corrupted proof
  // is the only defect: the forwarder call runs before verification, as in
  // pa-evm, and the verifier rejects the seal.
  it("rejects transaction with corrupted proof bytes", async () => {
    const corrupt = await loadFixture("corrupt_seal.json");
    return assertFails(
      settleUploaded(
        Buffer.from(corrupt.tx_b64, "base64"),
        buildSettleRemainingAccounts(deriveNullifierAccounts(corrupt.consumed_nullifiers_b64)),
      ),
      VERIFIER.malformedProof,
    );
  });

  it("rejects all-zero payload of valid length", () =>
    assertFails(settleUploaded(Buffer.alloc(validTx.length), nullifierAccounts), {
      program,
      error: "InvalidTransactionData",
    }));

  // --- Remaining accounts mutations ---

  it("rejects settlement with no remaining accounts", () =>
    assertFails(settleUploaded(validTx, []), { program, error: "InvalidTransactionData" }));

  // The first remaining account is read as the consumed resource's nullifier
  // marker, before any forwarder segment (UnregisteredForwarder is settle.ts's).
  it("rejects settlement with random remaining accounts", () => {
    const randomAccounts = Array.from({ length: 3 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isWritable: true,
      isSigner: false,
    }));
    return assertFails(settleUploaded(validTx, randomAccounts), { program, error: "NullifierPdaMismatch" });
  });
});
