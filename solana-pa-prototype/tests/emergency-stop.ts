/**
 * emergency_stop: it stops the adapter for good, and settlement is refused
 * afterwards.
 */
import { SystemProgram, Keypair, ComputeBudgetProgram } from "@solana/web3.js";
import { assert } from "chai";
import { emergencyStop } from "../client/instructions";
import { VERIFIER_ROUTER_ID } from "../client/verifier";
import {
  provider,
  program,
  paState,
  VERIFIER_PROGRAM_ID,
  routerPda,
  verifierEntryPda,
  DUMMY_ROOT_MARKER,
  ensureAdapterInitialized,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Emergency Stop E2E)", () => {
  const { funder, uploadTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  it("emergency_stop pauses protocol", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      JSON.stringify(stateBefore.lifecycle),
      JSON.stringify({ running: {} }),
      "Should be Running before emergency_stop",
    );

    await emergencyStop(program, provider.wallet.publicKey).rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      JSON.stringify(stateAfter.lifecycle),
      JSON.stringify({ stopped: {} }),
      "Should be Stopped after emergency_stop",
    );
  });

  it("rejects emergency_stop when already paused", async () => {
    try {
      await emergencyStop(program, provider.wallet.publicKey).rpc();
      assert.fail("expected emergency_stop to fail when already paused");
    } catch (e: any) {
      assertPAError(e, "AlreadyStopped");
    }
  });

  it("rejects settle when paused", async () => {
    const payer = Keypair.generate();
    await funder.fund(payer, 2);

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
        .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 })])
        .signers([payer])
        .rpc();
      assert.fail("expected settle to fail when paused");
    } catch (e: any) {
      assertPAError(e, "Stopped");
    }
  });

  it("rejects settle_from_txdata when paused", async () => {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);

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
