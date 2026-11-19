/**
 * The adapter upgraded in place across a state-layout change, as pa-evm's
 * UUPS proxy is upgraded to an implementation with other immutables: its
 * validator starts on the build deployed on devnet
 * (tests/fixtures/previous/protocol_adapter.so, state schema 4, which stored
 * the verifier router and selector at initialize). That build settles
 * through the verifier it stored, denies logic refs, retunes its expiry
 * bounds and pauses. The owner upgrades it through its own `upgrade` to this
 * build, which takes the router and selector from the build, and
 * `migrate_state` brings the state to schema 5, after which the owner runs
 * it, settling through the build's verifier, and upgrades it as before.
 */
import { BN, Idl, Program } from "@anchor-lang/core";
import { SystemProgram } from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../../client/constants";
import {
  denyLogicRefs,
  migrateState,
  pauseAdapter,
  setKindTreeRoot,
  unpauseAdapter,
  upgradeAdapter,
} from "../../client/instructions";
import { VERIFIER_ROUTER_ID, getVerifierEntryPda } from "../../client/verifier";
import { createdCommitmentsOf, loadPreviousFixture, parseSelectorFromFixture } from "../utils/fixtures";
import { assertFails, randomRef, sendSettlementV1, uploadTxData } from "../utils/helpers";
import { predictRootMarkerPda } from "../utils/merkle";
import {
  PROOF_SELECTOR,
  cpiEventsOf,
  deriveNullifierAccounts,
  paState,
  program,
  provider,
  settleFromTxDataBuilder,
  upgradeThroughProgram,
  useAdapterSuite,
  verifierForSelector,
} from "../utils/adapterSuite";
import { ADAPTER_SO } from "../utils/constants";

/** The previous build, through the production IDL of the release it is. */
const previous: Program<any> = new Program(
  JSON.parse(readFileSync("tests/fixtures/previous/protocol_adapter.json", "utf8")) as Idl,
  provider,
);

/**
 * The fixture the previous build settles, and the verifier it is initialized
 * with: the one the router routes the fixture's selector to.
 */
const previousFixture = loadPreviousFixture("batch_groth16_transfer_shape.json");
const previousSelector = parseSelectorFromFixture(previousFixture.selector);
const previousVerifierEntry = getVerifierEntryPda(previousSelector)[0];

describe("protocol-adapter (upgraded in place from schema 4)", () => {
  const { funder, settleUnsettledFixture } = useAdapterSuite({ initialize: false });
  const wallet = provider.wallet.publicKey;
  const deniedConsumed = randomRef();
  const deniedCreated = randomRef();
  let stateBefore: any;

  it("initializes the previous build with the verifier it stores", async () => {
    await previous.methods
      .initialize(wallet, VERIFIER_ROUTER_ID, Array.from(previousSelector))
      .accountsPartial({
        paState,
        verifierEntry: previousVerifierEntry,
        payer: wallet,
        systemProgram: SystemProgram.programId,
      })
      .rpc();
    const bytes = (await provider.connection.getAccountInfo(paState))!.data;
    assert.equal(bytes[8], PREVIOUS_SCHEMA_VERSION, "the previous build writes the previous schema version");
  });

  // The previous build settles, denies, retunes and pauses, so the state
  // carries a grown tree, both denylists and non-default bounds into the
  // migration. The transaction makes no external call.
  it("builds history through the previous build", async () => {
    const authority = await funder.fresh(2);
    const { uploadId, txData } = await uploadTxData(
      previous as any,
      paState,
      authority,
      Buffer.from(previousFixture.tx_b64, "base64"),
    );
    // The v1 transaction's header carries the compute budget and heap frame.
    const settle = await settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      await predictRootMarkerPda(previous as any, paState, createdCommitmentsOf(previousFixture)),
      deriveNullifierAccounts(previousFixture.consumed_nullifiers_b64),
      true,
      {
        program: previous,
        verifierEntry: previousVerifierEntry,
        verifierProgram: verifierForSelector(previousSelector).program,
      },
    ).instruction();
    await sendSettlementV1(provider, [settle], [authority]);
    await previous.methods
      .txdataClose(uploadId)
      .accountsPartial({ txData, authority: authority.publicKey, refund: authority.publicKey })
      .signers([authority])
      .rpc();
    await denyLogicRefs(previous as any, wallet, [
      { logicRef: deniedConsumed, consumed: true },
      { logicRef: deniedCreated, consumed: false },
    ]).rpc();
    await previous.methods
      .updateExpiryConfig(new BN(150), new BN(200_000))
      .accountsPartial({ paState, authority: wallet })
      .rpc();
    await pauseAdapter(previous as any, wallet).rpc();
    stateBefore = await (previous.account as any).paStateAccount.fetch(paState);
    assert.equal(
      stateBefore.nextIndex.toNumber(),
      createdCommitmentsOf(previousFixture).length,
      "the settlement appended",
    );
  });

  it("upgrades the program in place to this build through its own upgrade", () =>
    upgradeThroughProgram(previous, ADAPTER_SO, "upgraded", (buffer, spill) =>
      upgradeAdapter(previous as any, wallet, buffer, spill),
    ));

  // Every instruction that reads the state refuses the previous layout. Here
  // the typed load itself fails (AccountDidNotDeserialize): where this
  // layout reads `paused`, the previous one holds the stored selector's
  // first byte, which is not a bool, so nothing misreads the account.
  it("refuses an owner-only instruction before the state is migrated", () =>
    assertFails(setKindTreeRoot(program, wallet, Array.from(stateBefore.kindTreeRoot)).rpc(), {
      program,
      error: "AccountDidNotDeserialize",
      account: "pa_state",
    }));

  // Only the owner migrates, as only the EVM proxy's owner calls
  // upgradeToAndCall.
  it("rejects the migration from anyone but the owner", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(migrateState(program, stranger.publicKey).signers([stranger]).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
    });
  });

  it("migrates the state in place, keeping every field but the router and selector", async () => {
    const sig = await migrateState(program, wallet).rpc();
    const { events } = await cpiEventsOf(sig);
    // As OpenZeppelin's `reinitializer(n)`, the migration announces the
    // version it brought the state to.
    assert.deepEqual(
      events.map((e) => [e.name, e.data.version.toNumber()]),
      [["initialized", SCHEMA_VERSION]],
      "the migration announces the new schema version with Initialized",
    );

    const state: any = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.schemaVersion, SCHEMA_VERSION, "the state is in this build's layout");
    const { schemaVersion: _before, verifierRouter, proofSelector, ...carried } = stateBefore;
    assert.equal(verifierRouter.toBase58(), VERIFIER_ROUTER_ID.toBase58(), "the previous build stored the router");
    assert.deepEqual(Buffer.from(proofSelector), previousSelector, "and its selector");
    const { schemaVersion: _after, ...kept } = state;
    assert.equal(JSON.stringify(kept), JSON.stringify(carried), "every other field carries over");
  });

  it("rejects migrating twice", () =>
    assertFails(migrateState(program, wallet).rpc(), { program, error: "NotPreviousSchema" }));

  // The build's verifier replaces the one the previous build stored, as an
  // upgrade to an implementation with other immutables does on EVM.
  it("verifies with the build's router and selector", async () => {
    assert.equal((await program.methods.riscZeroVerifierRouter().view()).toBase58(), VERIFIER_ROUTER_ID.toBase58());
    assert.deepEqual(Buffer.from(await program.methods.riscZeroVerifierSelector().view()), PROOF_SELECTOR);
  });

  it("runs under its owner: unpauses, settles through the build's verifier and denies", async () => {
    await unpauseAdapter(program, wallet).rpc();
    await settleUnsettledFixture("batch_groth16_v2.json");
    const deprecated = randomRef();
    await denyLogicRefs(program, wallet, [{ logicRef: deprecated, consumed: false }]).rpc();
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused);
    assert.deepEqual(
      state.deniedCreatedLogicRefs.map((r: number[]) => Array.from(r)),
      [deniedCreated, deprecated],
      "the denylist for created resources keeps the previous build's entry",
    );
  });

  it("upgrades through the program from then on", async () => {
    await upgradeThroughProgram(program, ADAPTER_SO, "upgraded", (buffer, spill) =>
      upgradeAdapter(program, wallet, buffer, spill),
    );
    await setKindTreeRoot(program, wallet, Array.from(stateBefore.kindTreeRoot)).rpc();
  });
});
