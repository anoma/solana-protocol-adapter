/**
 * The adapter upgraded in place across a state-layout change, as pa-evm's
 * UUPS proxy is upgraded: its validator starts on the build deployed on
 * devnet (tests/fixtures/previous/protocol_adapter.so, state schema 3, one
 * denylist for consumed and created resources alike), which settles,
 * denies a logic ref, retunes its expiry bounds and pauses. The owner
 * upgrades it through its own `upgrade` to this build, and `migrate_state`
 * brings the state to schema 4, with the denied logic ref on both
 * denylists, after which the owner runs it and upgrades it as before.
 */
import { BN, Idl, Program } from "@anchor-lang/core";
import { assert } from "chai";
import { readFileSync } from "fs";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../../client/constants";
import {
  denyLogicRefs,
  initializeAdapter,
  migrateState,
  setKindTableCommitment,
  unpauseAdapter,
  upgradeAdapter,
} from "../../client/instructions";
import { VERIFIER_ROUTER_ID } from "../../client/verifier";
import { createdCommitmentsOf, loadFixture } from "../utils/fixtures";
import { assertFails, randomRef, sendV0, uploadTxData } from "../utils/helpers";
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
} from "../utils/adapterSuite";
import { ADAPTER_SO } from "../utils/constants";

/** The previous build, through its own production IDL (fetched from devnet with it). */
const previous: Program<any> = new Program(
  JSON.parse(readFileSync("tests/fixtures/previous/protocol_adapter.json", "utf8")) as Idl,
  provider,
);

describe("protocol-adapter (upgraded in place from schema 3)", () => {
  const { funder, settlementTable, settleUnsettledFixture } = useAdapterSuite({ initialize: false });
  const wallet = provider.wallet.publicKey;
  const deniedBefore = randomRef();
  let stateBefore: any;

  it("initializes the previous build", async () => {
    await initializeAdapter(previous, wallet, wallet, VERIFIER_ROUTER_ID, Array.from(PROOF_SELECTOR)).rpc();
    const bytes = (await provider.connection.getAccountInfo(paState))!.data;
    assert.equal(bytes[8], PREVIOUS_SCHEMA_VERSION, "the previous build writes the previous schema version");
  });

  // The previous build settles, denies, retunes and pauses, so the state
  // carries a grown tree, a denylist and non-default bounds into the
  // migration. The upload reads the state through the previous build's IDL;
  // the settle instruction is this build's, so the suite's builder sends it.
  // The transaction makes no external call: the previous build reads a
  // forwarder's raw return data, which this build's forwarders encode as a
  // Vec<u8>.
  it("builds history through the previous build", async () => {
    const fixture = await loadFixture("batch_groth16_transfer_shape.json");
    const authority = await funder.fresh(2);
    const { uploadId, txData } = await uploadTxData(
      previous as any,
      paState,
      authority,
      Buffer.from(fixture.tx_b64, "base64"),
    );
    const settle = await settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      await predictRootMarkerPda(previous as any, paState, createdCommitmentsOf(fixture)),
      deriveNullifierAccounts(fixture.consumed_nullifiers_b64),
    ).transaction();
    await sendV0(provider, settle.instructions, [authority], await settlementTable());
    await previous.methods
      .txdataClose(uploadId)
      .accountsPartial({ txData, authority: authority.publicKey, refund: authority.publicKey })
      .signers([authority])
      .rpc();
    await previous.methods.denyLogicRef(deniedBefore).accountsPartial({ paState, authority: wallet }).rpc();
    await previous.methods
      .updateExpiryConfig(new BN(150), new BN(200_000))
      .accountsPartial({ paState, authority: wallet })
      .rpc();
    await previous.methods.pause().accountsPartial({ paState, authority: wallet }).rpc();
    stateBefore = await (previous.account as any).paStateAccount.fetch(paState);
    assert.equal(stateBefore.nextIndex.toNumber(), createdCommitmentsOf(fixture).length, "the settlement appended");
  });

  it("upgrades the program in place to this build through its own upgrade", () =>
    upgradeThroughProgram(previous, ADAPTER_SO, "upgradedEvent", (buffer, spill) =>
      upgradeAdapter(previous as any, wallet, buffer, spill),
    ));

  // Every instruction that reads the state refuses the previous layout. Here
  // the typed load itself fails (AccountDidNotDeserialize): the previous
  // layout lacks the second denylist's length, so it does not read as this
  // one, and nothing misreads the account.
  it("refuses an owner-only instruction before the state is migrated", () =>
    assertFails(setKindTableCommitment(program, wallet, Array.from(stateBefore.kindTableCommitment)).rpc(), {
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

  it("migrates the state in place, the denied logic ref on both denylists", async () => {
    const sig = await migrateState(program, wallet).rpc();
    const { events } = await cpiEventsOf(sig);
    assert.deepEqual(
      events.map((e) => [e.name, Array.from(e.data.logicRef), e.data.consumed]),
      [
        ["logicRefDeniedEvent", deniedBefore, true],
        ["logicRefDeniedEvent", deniedBefore, false],
      ],
      "the migration announces the entry on each denylist, as deny_logic_refs would",
    );

    const state: any = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.schemaVersion, SCHEMA_VERSION, "the state is in this build's layout");
    for (const field of [
      "bump",
      "owner",
      "verifierRouter",
      "proofSelector",
      "kindTableCommitment",
      "paused",
      "root",
      "nextIndex",
      "currentDepth",
      "frontier",
      "minExpirySlots",
      "maxExpirySlots",
    ]) {
      assert.equal(JSON.stringify(state[field]), JSON.stringify(stateBefore[field]), `${field} carries over`);
    }
    for (const field of ["deniedConsumedLogicRefs", "deniedCreatedLogicRefs"]) {
      assert.deepEqual(
        state[field].map((r: number[]) => Array.from(r)),
        [deniedBefore],
        `${field} holds the previous denylist`,
      );
    }
  });

  it("rejects migrating twice", () =>
    assertFails(migrateState(program, wallet).rpc(), { program, error: "NotPreviousSchema" }));

  it("runs under its owner: unpauses, settles and deprecates", async () => {
    await unpauseAdapter(program, wallet).rpc();
    await settleUnsettledFixture("batch_groth16_v2.json");
    const deprecated = randomRef();
    await denyLogicRefs(program, wallet, [{ logicRef: deprecated, consumed: false }]).rpc();
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused);
    assert.deepEqual(
      state.deniedCreatedLogicRefs.map((r: number[]) => Array.from(r)),
      [deniedBefore, deprecated],
      "the denylist for created resources keeps the previous build's entry",
    );
  });

  it("upgrades through the program from then on", async () => {
    await upgradeThroughProgram(program, ADAPTER_SO, "upgradedEvent", (buffer, spill) =>
      upgradeAdapter(program, wallet, buffer, spill),
    );
    await setKindTableCommitment(program, wallet, Array.from(stateBefore.kindTableCommitment)).rpc();
  });
});
