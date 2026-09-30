/**
 * emergency_stop: it stops the adapter for good, and settlement is refused
 * afterwards.
 */
import { assert } from "chai";
import { assertFails } from "./utils/helpers";
import {
  program,
  paState,
  DUMMY_ROOT_MARKER,
  settleBuilder,
  settleFromTxDataBuilder,
  stopAdapter,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Emergency Stop E2E)", () => {
  const { funder, uploadTxData } = useAdapterSuite();

  it("emergency_stop pauses protocol", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      JSON.stringify(stateBefore.lifecycle),
      JSON.stringify({ running: {} }),
      "Should be Running before emergency_stop",
    );

    await stopAdapter();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      JSON.stringify(stateAfter.lifecycle),
      JSON.stringify({ stopped: {} }),
      "Should be Stopped after emergency_stop",
    );
  });

  it("rejects emergency_stop when already paused", async () => {
    await assertFails(stopAdapter(), { program, error: "AlreadyStopped" });
  });

  it("rejects settle when paused", async () => {
    const payer = await funder.fresh(2);

    // Use a small garbage payload — the paused check fires before deserialization,
    // so any payload suffices. The full fixture is too large for a single settle instruction.
    await assertFails(
      settleBuilder(payer.publicKey, Buffer.from([0, 1, 2, 3]), [], false)
        .signers([payer])
        .rpc(),
      { program, error: "Stopped" },
    );
  });

  it("rejects settle_from_txdata when paused", async () => {
    const authority = await funder.fresh(2);

    // Paused check fires before deserialization — minimal payload suffices
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from([0, 1, 2, 3]));

    await assertFails(
      settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, []).signers([authority]).rpc(),
      { program, error: "Stopped" },
    );
  });
});
