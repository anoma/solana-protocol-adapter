/**
 * `initialize`: only the program's upgrade authority may call it (AUTH-01),
 * and it starts every deployment on the empty kind table, announcing it as
 * pa-evm's initializer does. Needs an adapter that was never initialized:
 * it runs first, on the fresh deployment the rest of the suite builds on.
 */
import { PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { EMPTY_KIND_TABLE_COMMITMENT } from "../../client/constants";
import { initializeAdapter } from "../../client/instructions";
import { PAUSED_MOCK_SELECTOR, VERIFIER_ROUTER_ID, getVerifierEntryPda } from "../../client/verifier";
import { EMPTY_TREE_ROOT_INITIAL } from "../utils/constants";
import { assertFails } from "../utils/helpers";
import {
  PROOF_SELECTOR,
  buildInitialize,
  cpiEventsOf,
  paState,
  paStateExists,
  program,
  provider,
  useAdapterSuite,
} from "../utils/adapterSuite";

describe("protocol-adapter (initialize)", () => {
  const { funder } = useAdapterSuite({ initialize: false });

  it("rejects initialization by a non-upgrade-authority signer", async () => {
    // Needs an adapter that was never initialized — otherwise the `init`
    // constraint on `pa_state` would fail with
    // "already in use" before the AUTH-01 constraint on `program_data` is
    // ever reached, which would prove nothing about this fix.
    assert.isFalse(
      await paStateExists(),
      "PAState is already initialized; the AUTH-01 rejection test needs a " +
        "validator on which the adapter was never initialized",
    );

    const stranger = await funder.fresh(2);

    // The error must be our Unauthorized code, raised by the adapter for the
    // `program_data` account's upgrade-authority constraint specifically —
    // not by account resolution, not by the `program` constraint, and not by
    // any earlier check. Anchor names the offending account in its log line,
    // which is what distinguishes "rejected for the right reason" from
    // "rejected before the constraint was ever evaluated".
    await assertFails(buildInitialize(stranger.publicKey).signers([stranger]).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });

    // The rejected transaction must not have left PAState initialized.
    assert.isFalse(await paStateExists(), "PAState must remain uninitialized after the rejected call");
  });

  // Mirrors pa-evm's constructor: ZeroRiscZeroVerifier{Router,Selector}NotAllowed.
  it("rejects a zero verifier router", () =>
    assertFails(
      initializeAdapter(program, provider.wallet.publicKey, PublicKey.default, Array.from(PROOF_SELECTOR)).rpc(),
      { program, error: "ZeroVerifierRouterNotAllowed" },
    ));

  it("rejects a zero proof selector", () =>
    assertFails(initializeAdapter(program, provider.wallet.publicKey, VERIFIER_ROUTER_ID, [0, 0, 0, 0]).rpc(), {
      program,
      error: "ZeroProofSelectorNotAllowed",
    }));

  // Mirrors pa-evm's initializer's sanity check: RiscZeroVerifierPaused. The
  // localnet router registers PAUSED_MOCK_SELECTOR with its estop set.
  it("rejects a verifier the router has paused", () =>
    assertFails(
      initializeAdapter(program, provider.wallet.publicKey, VERIFIER_ROUTER_ID, Array.from(PAUSED_MOCK_SELECTOR)).rpc(),
      { program, error: "RiscZeroVerifierPaused" },
    ));

  it("rejects an account other than the router's verifier entry for the selector", () =>
    assertFails(
      initializeAdapter(program, provider.wallet.publicKey, VERIFIER_ROUTER_ID, Array.from(PROOF_SELECTOR))
        .accountsPartial({ verifierEntry: getVerifierEntryPda(PAUSED_MOCK_SELECTOR)[0] })
        .rpc(),
      { program, error: "InvalidVerifierEntry" },
    ));

  it("stores the empty kind table and emits the initial root and the kind table, as pa-evm's initializer does", async () => {
    assert.isFalse(await paStateExists(), "this test initializes the adapter, so it must start uninitialized");

    const sig = await buildInitialize(provider.wallet.publicKey).rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(
      Buffer.from(state.kindTableCommitment),
      EMPTY_KIND_TABLE_COMMITMENT,
      "initialize must store the empty kind table's commitment",
    );
    assert.isFalse(state.paused, "the adapter starts unpaused");
    assert.equal(state.nextIndex.toNumber(), 0, "the tree starts empty");
    assert.equal(state.currentDepth, 1, "the tree starts at depth 1");
    assert.deepEqual(Buffer.from(state.root as number[]), EMPTY_TREE_ROOT_INITIAL, "the root is the empty tree's");
    // pa-evm's initializer adds the empty tree's root (CommitmentTreeRootAdded)
    // and installs the empty kind table (KindTableCommitmentUpdated), in that
    // order. Its ownership is the upgrade authority the loader records, so
    // there is no ownership event.
    const { events } = await cpiEventsOf(sig);
    assert.deepEqual(
      events.map((e) => e.name),
      ["commitmentTreeRootAddedEvent", "kindTableCommitmentUpdatedEvent"],
      "initialize emits pa-evm's initializer events in order",
    );
    assert.deepEqual(Buffer.from(events[0].data.root), EMPTY_TREE_ROOT_INITIAL, "the empty tree's root");
    assert.deepEqual(Buffer.from(events[1].data.kindTableCommitment), EMPTY_KIND_TABLE_COMMITMENT);
  });
});
