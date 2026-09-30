/**
 * close_markers_batch: refused while the adapter runs, and on a stopped
 * adapter it closes the markers and refunds their rent. The before hook
 * settles the primary fixture, leaving markers to close.
 */
import { LAMPORTS_PER_SOL } from "@solana/web3.js";
import { assert } from "chai";
import { closeAllMarkers, closeMarkersBatch } from "../client/devTeardown";
import { assertFails } from "./utils/helpers";
import { provider, program, paState, stopAdapter, useAdapterSuite } from "./utils/adapterSuite";

// ── Close instruction tests ──────────────────────────────────────────────

describe("protocol-adapter (Close instructions)", () => {
  const { funder, settleFixture } = useAdapterSuite();

  before(() => settleFixture("batch_groth16.json"));

  it("close_markers_batch fails when PA is not stopped", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(state.lifecycle, { running: {} }, "PA should be Running at start of test");

    // The markers the before hook's settlement created
    const markers = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.ok(markers.length > 0, "Should have markers to close");

    await assertFails(
      closeMarkersBatch(
        program,
        provider.wallet.publicKey,
        markers.map(({ pubkey }) => pubkey),
      ).rpc(),
      { program, error: "NotStopped" },
    );
  });

  describe("on a stopped adapter", () => {
    before(stopAdapter);

    it("close_markers_batch closes marker PDAs and refunds rent", async () => {
      const markersBefore = (
        await provider.connection.getProgramAccounts(program.programId, {
          filters: [{ dataSize: 0 }],
        })
      ).length;
      assert.isAbove(markersBefore, 0, "the before hook's settlement leaves markers to close");
      const balanceBefore = await provider.connection.getBalance(provider.wallet.publicKey);

      assert.equal(await closeAllMarkers(program, provider.wallet.publicKey), markersBefore, "every marker is closed");

      // Verify markers are gone
      const markersAfter = await provider.connection.getProgramAccounts(program.programId, {
        filters: [{ dataSize: 0 }],
      });
      assert.equal(markersAfter.length, 0, "All markers should be closed");

      const balanceAfter = await provider.connection.getBalance(provider.wallet.publicKey);
      assert.ok(balanceAfter > balanceBefore, "Authority should have received rent refund");
      console.log(
        `    Closed ${markersBefore} markers, recovered ${((balanceAfter - balanceBefore) / LAMPORTS_PER_SOL).toFixed(6)} SOL`,
      );
    });

    it("close_markers_batch rejects non-authority", async () => {
      const fakeAuthority = await funder.fresh(1);

      await assertFails(closeMarkersBatch(program, fakeAuthority.publicKey, []).signers([fakeAuthority]).rpc(), {
        program,
        error: "Unauthorized",
      });
    });
  });
});
