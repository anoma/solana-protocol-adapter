/**
 * Security mutation tests: submit crafted/mutated transactions to an
 * initialized adapter and verify each is rejected by the check that owns
 * that malformation. Payloads that fit a transaction go inline through
 * `settle`; the full-size ones go through a TxData upload.
 */
import { ComputeBudgetProgram, Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { VERIFIER_ROUTER_ID } from "../../client/verifier";
import { loadFixture } from "../utils/fixtures";
import { assertFails } from "../utils/helpers";
import {
  DUMMY_ROOT_MARKER,
  VERIFIER,
  VERIFIER_PROGRAM_ID,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  fixture,
  paState,
  program,
  routerPda,
  useAdapterSuite,
  verifierEntryPda,
} from "../utils/adapterSuite";

type Meta = { pubkey: PublicKey; isWritable: boolean; isSigner: boolean };

/** Truncate a buffer. Returns a new buffer. */
function truncate(buf: Buffer, len: number): Buffer {
  return Buffer.from(buf.subarray(0, len));
}

describe("Security: mutation-based settle tests", () => {
  const { funder, settleFixtureViaTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  const validTx = Buffer.from(fixture.tx_b64, "base64");
  const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);

  /** Submit `payload` inline via `settle`. */
  async function settleInline(payload: Buffer, remainingAccounts: Meta[] = []): Promise<string> {
    const payer = Keypair.generate();
    await funder.fund(payer, 2);
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
      .remainingAccounts(remainingAccounts)
      .preInstructions([
        ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
      ])
      .signers([payer])
      .rpc();
  }

  /** Upload `payload` to TxData and settle it: full-size payloads exceed an inline `settle`. */
  const settleUploaded = (payload: Buffer, remainingAccounts: Meta[]) =>
    settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });

  // --- Empty / truncated payloads ---

  it("rejects empty payload", () =>
    assertFails(settleInline(Buffer.alloc(0)), { program, error: "InvalidTransactionData" }));

  it("rejects single-byte payload", () =>
    assertFails(settleInline(Buffer.from([0x00])), { program, error: "InvalidTransactionData" }));

  it("rejects truncated valid transaction", () =>
    assertFails(settleInline(truncate(validTx, 64)), { program, error: "InvalidTransactionData" }));

  // --- Zero-action transaction (SEC-006 regression) ---

  it("rejects zero-action transaction", () =>
    assertFails(settleInline(Buffer.from(loadFixture("zero_action.json").tx_b64, "base64")), {
      program,
      error: "InvalidTransactionData",
    }));

  // --- Proof mutations ---

  // The primary fixture with its seal's pi_c[0] flipped and the selector
  // intact (fixture-gen's corrupt_seal.json variant): the router routes it
  // to the fixture's verifier, which rejects the malformed point.
  it("rejects transaction with corrupted proof bytes", () => {
    const corrupt = loadFixture("corrupt_seal.json");
    return assertFails(
      settleUploaded(Buffer.from(corrupt.tx_b64, "base64"), deriveNullifierAccounts(corrupt.consumed_nullifiers_b64)),
      { program: VERIFIER.program, code: VERIFIER.malformedProofCode },
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

  it("rejects settlement with random remaining accounts", () => {
    const randomAccounts = Array.from({ length: 3 }, () => ({
      pubkey: Keypair.generate().publicKey,
      isWritable: true,
      isSigner: false,
    }));
    return assertFails(settleUploaded(validTx, randomAccounts), { program, error: "UnregisteredForwarder" });
  });
});
