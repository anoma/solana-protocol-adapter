/**
 * `initialize`: only the program's upgrade authority may call it (AUTH-01),
 * and it starts every deployment on the empty kind table, announcing it as
 * pa-evm's initializer does. Needs an adapter that was never initialized,
 * which the file's fresh validator provides.
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import {
  errorHaystack,
} from "./utils";
import { EMPTY_KIND_TABLE_COMMITMENT } from "./utils/constants";
import {
  buildInitialize,
  cpiEventsOf,
  paState,
  paStateExists,
  program,
  provider,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (initialize)", () => {
  const { funder } = useAdapterSuite();

  it("rejects initialization by a non-upgrade-authority signer", async () => {
    // Needs an adapter that was never initialized — otherwise the `init`
    // constraint on `pa_state` would fail with
    // "already in use" before the AUTH-01 constraint on `program_data` is
    // ever reached, which would prove nothing about this fix.
    assert.isFalse(
      await paStateExists(),
      "PAState is already initialized; the AUTH-01 rejection test needs a " +
        "validator on which the adapter was never initialized"
    );

    const stranger = Keypair.generate();
    await funder.fund(stranger, 2);

    let caught: any = null;
    try {
      await buildInitialize(stranger.publicKey).signers([stranger]).rpc();
    } catch (e: any) {
      caught = e;
    }
    assert.isNotNull(
      caught,
      "expected initialization by a non-upgrade-authority signer to fail"
    );

    // The error must be our Unauthorized code, and it must have been raised by
    // the `program_data` account's upgrade-authority constraint specifically —
    // not by account resolution, not by the `program` constraint, and not by
    // any earlier check. Anchor names the offending account in its log line,
    // which is what distinguishes "rejected for the right reason" from
    // "rejected before the constraint was ever evaluated".
    assertPAError(caught, "Unauthorized");
    assert.match(
      errorHaystack(caught),
      /AnchorError caused by account: program_data/,
      "Unauthorized must originate from the program_data upgrade-authority " +
        `constraint. Got:\n${errorHaystack(caught)}`
    );

    // The rejected transaction must not have left PAState initialized.
    assert.isFalse(
      await paStateExists(),
      "PAState must remain uninitialized after the rejected call"
    );
  });

  it("stores the empty kind table and emits KindTableCommitmentUpdated", async () => {
    assert.isFalse(await paStateExists(), "this test initializes the adapter, so it must start uninitialized");

    const sig = await buildInitialize(provider.wallet.publicKey).rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(
      Buffer.from(state.kindTableCommitment),
      EMPTY_KIND_TABLE_COMMITMENT,
      "initialize must store the empty kind table's commitment"
    );
    const { events } = await cpiEventsOf(sig);
    assert.deepEqual(
      events.map((e) => [e.name, Buffer.from(e.data.kindTableCommitment).toString("hex")]),
      [["kindTableCommitmentUpdatedEvent", EMPTY_KIND_TABLE_COMMITMENT.toString("hex")]],
      "initialize must emit exactly one KindTableCommitmentUpdated carrying the empty kind table, as pa-evm's initializer does"
    );
  });
});
