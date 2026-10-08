/**
 * The owner's logic-ref denylists, as pa-evm's denyLogicRefs: one for
 * consumed resources and one for created resources. A logic ref on the
 * created side only is deprecated (its resources are still consumed); one on
 * both is denied. An entry is never removed, and these tests deny the logic
 * almost every fixture carries, so this runs among the suite's last files.
 */
import { assert } from "chai";
import { DeniedLogicRef, denyLogicRefs } from "../../client/instructions";
import { requireFixture } from "../utils/fixtures";
import { randomRef, assertFails } from "../utils/helpers";
import {
  provider,
  program,
  paState,
  cpiEventsOf,
  DUMMY_ROOT_MARKER,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  deriveNullifierAccounts,
  useAdapterSuite,
} from "../utils/adapterSuite";

describe("protocol-adapter (logic-ref denylists)", () => {
  const { funder, settleUnsettledFixture, settleFixture, settleFixtureViaTxData } = useAdapterSuite();
  const authority = provider.wallet.publicKey;
  const denylists = async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    const refs = (list: number[][]) => list.map((r) => Array.from(r));
    return { consumed: refs(state.deniedConsumedLogicRefs), created: refs(state.deniedCreatedLogicRefs) };
  };

  /** Submit `fixtureName` for settlement, whatever its nullifiers' state. */
  const submit = (fixtureName: string) => {
    const f = requireFixture(fixtureName);
    return settleFixtureViaTxData(
      Buffer.from(f.tx_b64, "base64"),
      buildSettleRemainingAccounts(deriveNullifierAccounts(f.consumed_nullifiers_b64)),
      { newRootMarker: DUMMY_ROOT_MARKER },
    );
  };

  it("rejects a denial by anyone but the owner", async () => {
    const intruder = await funder.fresh(1);
    await assertFails(
      denyLogicRefs(program, intruder.publicKey, [{ logicRef: randomRef(), consumed: true }])
        .signers([intruder])
        .rpc(),
      { program, error: "OwnableUnauthorizedAccount" },
    );
  });

  it("rejects denying the zero logic ref", () =>
    assertFails(denyLogicRefs(program, authority, [{ logicRef: Array(32).fill(0), consumed: false }]).rpc(), {
      program,
      error: "ZeroLogicRefNotAllowed",
    }));

  // pa-evm's test_denyLogicRefs_denies_one_logic_ref_and_deprecates_another_in_one_call.
  it("denies one logic ref and deprecates another in one call, announcing each entry", async () => {
    const before = await denylists();
    const [denied, deprecated] = [randomRef(), randomRef()];
    const entries: DeniedLogicRef[] = [
      { logicRef: denied, consumed: true },
      { logicRef: denied, consumed: false },
      { logicRef: deprecated, consumed: false },
    ];
    const sig = await denyLogicRefs(program, authority, entries).rpc();

    assert.deepEqual(
      await denylists(),
      { consumed: [...before.consumed, denied], created: [...before.created, denied, deprecated] },
      "the denied logic ref is on both denylists, the deprecated one on the one for created resources only",
    );
    const events = (await cpiEventsOf(sig)).events.filter((e) => e.name === "logicRefDeniedEvent");
    assert.deepEqual(
      events.map((e) => ({ logicRef: Array.from(e.data.logicRef), consumed: e.data.consumed })),
      entries,
      "one LogicRefDenied event per entry, in order",
    );
  });

  // pa-evm's LogicRefAlreadyDenied(logicRef, consumed): the check is per
  // denylist, and a call with a refused entry adds none of its entries.
  it("rejects adding a logic ref to a denylist it is already on, and nothing of that call", async () => {
    const ref = randomRef();
    await denyLogicRefs(program, authority, [{ logicRef: ref, consumed: false }]).rpc();
    const before = await denylists();
    const fresh = randomRef();
    await assertFails(
      denyLogicRefs(program, authority, [
        { logicRef: fresh, consumed: false },
        { logicRef: ref, consumed: false },
      ]).rpc(),
      { program, error: "LogicRefAlreadyDenied" },
    );
    assert.deepEqual(await denylists(), before, "the call's earlier entry is not added");

    await denyLogicRefs(program, authority, [{ logicRef: ref, consumed: true }]).rpc();
    assert.deepEqual((await denylists()).consumed, [...before.consumed, ref], "the other denylist takes it");
  });

  // pa-evm checks each consumed resource against the denylist for consumed
  // resources and each created one against the one for created resources.
  // Every batch fixture's resources carry the passthrough logic; this
  // file's own settlement reports it. The denylists are checked before any
  // nullifier is recorded, so a refused fixture stays unspent.
  describe("settlement", () => {
    let passthrough: number[];

    before(async () => {
      const sig = await settleUnsettledFixture("batch_groth16_denylist.json");
      const [action] = (await cpiEventsOf(sig)).events.filter((e) => e.name === "actionExecutedEvent");
      passthrough = Array.from(action.data.consumedLogicRefs[0]);
      await settleFixture("batch_groth16_consume_only.json");
    });

    // pa-evm's test_execute_reverts_if_a_created_resource_carries_a_deprecated_logic_ref.
    it("rejects a settlement that creates a resource carrying a deprecated logic ref", async () => {
      await denyLogicRefs(program, authority, [{ logicRef: passthrough, consumed: false }]).rpc();
      await assertFails(submit("batch_groth16_rejected.json"), { program, error: "ResourceWithDeniedLogicRef" });
      await assertFixtureUnsettled("batch_groth16_rejected.json");
    });

    // pa-evm's test_execute_consumes_a_resource_that_carries_a_deprecated_logic_ref.
    // The consume-only fixture's one resource is consumed and is settled
    // already, so a resubmission passes the denylists and reaches nullifier
    // creation.
    it("lets a settlement consume a resource carrying a deprecated logic ref", () =>
      assertFails(submit("batch_groth16_consume_only.json"), { program, error: "PreExistingNullifier" }));

    // pa-evm's test_execute_reverts_if_a_consumed_resource_carries_a_denied_logic_ref.
    it("rejects a settlement that consumes a resource carrying a denied logic ref", async () => {
      await denyLogicRefs(program, authority, [{ logicRef: passthrough, consumed: true }]).rpc();
      await assertFails(submit("batch_groth16_consume_only.json"), {
        program,
        error: "ResourceWithDeniedLogicRef",
      });
    });
  });
});
