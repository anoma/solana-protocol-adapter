/**
 * close_markers_batch: refused while the adapter is not paused, and on a paused
 * adapter it closes the markers and refunds their rent. Closing every marker
 * forgets every spent nullifier and retained root, so this runs among the
 * suite's last files. The before hook settles the resubmitted fixture unless
 * it is settled already, so there are markers to close.
 */
import { LAMPORTS_PER_SOL } from "@solana/web3.js";
import { assert } from "chai";
import { localCloseAllMarkers, localCloseMarkersBatch } from "./utils/localOnly";
import { assertFails } from "./utils/helpers";
import { provider, program, paState, ensurePaused, useAdapterSuite } from "./utils/adapterSuite";

// ── Close instruction tests ──────────────────────────────────────────────

describe("protocol-adapter (Close instructions)", () => {
  const { funder, settleFixture } = useAdapterSuite();

  before(() => settleFixture("batch_groth16_resubmitted.json"));

  it("close_markers_batch fails when the PA is not paused", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused, "the PA is not paused at the start of the test");

    // The markers the deployment's settlements created
    const markers = await provider.connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    assert.ok(markers.length > 0, "Should have markers to close");

    await assertFails(
      localCloseMarkersBatch(
        program,
        provider.wallet.publicKey,
        markers.map(({ pubkey }) => pubkey),
      ).rpc(),
      { program, error: "ExpectedPause" },
    );
  });

  describe("on a paused adapter", () => {
    before(ensurePaused);

    it("close_markers_batch closes marker PDAs and refunds rent", async () => {
      const markersBefore = (
        await provider.connection.getProgramAccounts(program.programId, {
          filters: [{ dataSize: 0 }],
        })
      ).length;
      assert.isAbove(markersBefore, 0, "the deployment's settlements leave markers to close");
      const balanceBefore = await provider.connection.getBalance(provider.wallet.publicKey);

      assert.equal(
        await localCloseAllMarkers(program, provider.wallet.publicKey),
        markersBefore,
        "every marker is closed",
      );

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

      await assertFails(localCloseMarkersBatch(program, fakeAuthority.publicKey, []).signers([fakeAuthority]).rpc(), {
        program,
        error: "Unauthorized",
      });
    });
  });
});
