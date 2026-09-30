/**
 * `settle` with inline transaction data, and the double-spend rejection.
 * The before hook settles the primary fixture, whose nullifiers the
 * duplicate test re-submits.
 */
import { SystemProgram, Keypair, ComputeBudgetProgram } from "@solana/web3.js";
import { assert } from "chai";
import { VERIFIER_ROUTER_ID } from "../client/verifier";
import { loadFixture } from "./utils/fixtures";
import { assertFails } from "./utils/helpers";
import {
  program,
  paState,
  fixture,
  VERIFIER_PROGRAM_ID,
  routerPda,
  verifierEntryPda,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  buildSettleRemainingAccounts,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Direct settle & duplicate nullifier)", () => {
  const { funder, uploadTxData, settleFixture } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
    await settleFixture("batch_groth16.json");
  });

  it("rejects garbage transaction_data via settle", async () => {
    const payer = Keypair.generate();
    await funder.fund(payer, 2);

    await assertFails(
      program.methods
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
        .rpc(),
      { program, error: "InvalidTransactionData" },
    );
  });

  it("rejects empty transaction (zero actions) via settle", async () => {
    const payer = Keypair.generate();
    await funder.fund(payer, 2);

    // The fixture's aggregated transaction with its instance's action list
    // emptied (fixture-gen's zero_action.json error variant, SEC-006
    // regression): deserializes cleanly, rejected by the PA's empty-instance
    // check.
    const emptyTx = Buffer.from(loadFixture("zero_action.json").tx_b64, "base64");

    await assertFails(
      program.methods
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
        .rpc(),
      { program, error: "InvalidTransactionData" },
    );
  });

  it("rejects duplicate nullifier (double-spend) via settle_from_txdata", async () => {
    // The before hook consumed the fixture's nullifiers. Re-uploading the same tx
    // to a fresh TxData and trying to settle must fail at nullifier creation
    // because those nullifier PDAs already exist.
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

    const tx = Buffer.from(fixture.tx_b64, "base64");
    const { uploadId, txData } = await uploadTxData(authority, tx);

    // The SAME nullifier PDAs (created by the before hook's settlement)
    const nullifierAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const allRemainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

    await assertFails(
      program.methods
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
        .rpc(),
      { program, error: "DuplicateNullifier" },
    );
  });
});
