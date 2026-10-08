/**
 * pause / unpause, as pa-evm's owner-only `pause()` and `unpause()`
 * (OpenZeppelin Pausable): a pause refuses settlement until the owner
 * unpauses, each is refused in the wrong state, and each is announced. Also
 * `risc_zero_verifier_paused`, pa-evm's `riscZeroVerifierPaused()`.
 */
import { assert } from "chai";
import { pauseAdapter, unpauseAdapter } from "../client/instructions";
import { PAUSED_MOCK_SELECTOR, getVerifierEntryPda } from "../client/verifier";
import { assertFails } from "./utils/helpers";
import {
  program,
  paState,
  provider,
  cpiEventsOf,
  DUMMY_ROOT_MARKER,
  settleBuilder,
  settleFromTxDataBuilder,
  verifierEntryPda,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (pause) @localnet", () => {
  const { funder, uploadTxData, settleUnsettledFixture } = useAdapterSuite();
  const owner = provider.wallet.publicKey;

  const paused = async () => (await program.account.paStateAccount.fetch(paState)).paused;
  const eventsOf = async (sig: string) =>
    (await cpiEventsOf(sig)).events.map((e) => [e.name, e.data.account.toBase58()]);

  // The deployment is shared with every later file: a failure between the
  // pause and the unpause must not leave it paused.
  after(async () => {
    if (await paused()) await unpauseAdapter(program, owner).rpc();
  });

  it("reports the deployment's verifier as not paused", async () => {
    assert.isFalse(
      await program.methods
        .riscZeroVerifierPaused()
        .accountsPartial({ paState, verifierEntry: verifierEntryPda })
        .view(),
    );
  });

  it("refuses to read an account other than the deployment's verifier entry", () =>
    assertFails(
      program.methods
        .riscZeroVerifierPaused()
        .accountsPartial({ paState, verifierEntry: getVerifierEntryPda(PAUSED_MOCK_SELECTOR)[0] })
        .rpc(),
      { program, error: "InvalidVerifierEntry" },
    ));

  it("rejects unpause while not paused", async () => {
    assert.isFalse(await paused(), "the adapter starts unpaused");
    await assertFails(unpauseAdapter(program, owner).rpc(), { program, error: "ExpectedPause" });
  });

  it("pauses and announces it", async () => {
    const sig = await pauseAdapter(program, owner).rpc();
    assert.isTrue(await paused());
    assert.deepEqual(await eventsOf(sig), [["pausedEvent", owner.toBase58()]]);
  });

  it("rejects pause while paused", () =>
    assertFails(pauseAdapter(program, owner).rpc(), { program, error: "EnforcedPause" }));

  it("rejects settle while paused", async () => {
    const payer = await funder.fresh(2);
    // The paused check fires before deserialization, so any payload suffices.
    await assertFails(
      settleBuilder(payer.publicKey, Buffer.from([0, 1, 2, 3]), [], false)
        .signers([payer])
        .rpc(),
      { program, error: "EnforcedPause" },
    );
  });

  it("rejects settle_from_txdata while paused", async () => {
    const authority = await funder.fresh(2);
    const { uploadId, txData } = await uploadTxData(authority, Buffer.from([0, 1, 2, 3]));
    await assertFails(
      settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, []).signers([authority]).rpc(),
      { program, error: "EnforcedPause" },
    );
  });

  it("unpauses, announces it, and settles again", async () => {
    const sig = await unpauseAdapter(program, owner).rpc();
    assert.isFalse(await paused());
    assert.deepEqual(await eventsOf(sig), [["unpausedEvent", owner.toBase58()]]);
    await settleUnsettledFixture("batch_groth16_unpaused.json");
  });
});
