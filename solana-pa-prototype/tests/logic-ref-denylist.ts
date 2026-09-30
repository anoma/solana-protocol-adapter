/**
 * The authority's logic-ref denylist, as pa-evm's denyLogicRef: a denied
 * logic ref stays denied, and no settlement consumes or creates a resource
 * carrying it.
 */
import { assert } from "chai";
import { denyLogicRef } from "../client/instructions";
import { makeFunder, randomRef, assertFails } from "./utils/helpers";
import { provider, program, paState, cpiEventsOf, assertFixtureUnsettled, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (logic-ref denylist)", () => {
  const { settleUnsettledFixture } = useAdapterSuite();
  const funder = makeFunder(provider);
  const authority = provider.wallet.publicKey;

  it("rejects a denial by anyone but the authority", async () => {
    const intruder = await funder.fresh(1);
    await assertFails(denyLogicRef(program, intruder.publicKey, randomRef()).signers([intruder]).rpc(), {
      program,
      error: "Unauthorized",
    });
  });

  it("rejects denying the zero logic ref", () =>
    assertFails(denyLogicRef(program, authority, Array(32).fill(0)).rpc(), {
      program,
      error: "ZeroLogicRefNotAllowed",
    }));

  it("denies a logic ref and announces it", async () => {
    const ref = randomRef();
    const sig = await denyLogicRef(program, authority, ref).rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(
      state.deniedLogicRefs.map((r: number[]) => Array.from(r)),
      [ref],
      "the state lists the denied logic ref",
    );
    const events = (await cpiEventsOf(sig)).events.filter((e) => e.name === "logicRefDeniedEvent");
    assert.lengthOf(events, 1, "one LogicRefDenied event");
    assert.deepEqual(Array.from(events[0].data.logicRef), ref);
  });

  it("rejects denying a logic ref twice", async () => {
    const [ref] = (await program.account.paStateAccount.fetch(paState)).deniedLogicRefs;
    await assertFails(denyLogicRef(program, authority, Array.from(ref)).rpc(), {
      program,
      error: "LogicRefAlreadyDenied",
    });
  });

  // pa-evm checks every consumed and created resource: a transaction whose
  // resources carry a denied logic ref does not settle. Every batch
  // fixture's resources carry the passthrough logic; the first settlement
  // reports it.
  it("rejects a settlement whose resources carry a denied logic ref", async () => {
    const sig = await settleUnsettledFixture("batch_groth16_v2.json");
    const [action] = (await cpiEventsOf(sig)).events.filter((e) => e.name === "actionExecutedEvent");
    const passthrough: number[] = Array.from(action.data.consumedLogicRefs[0]);

    await denyLogicRef(program, authority, passthrough).rpc();
    await assertFails(settleUnsettledFixture("batch_groth16.json"), { program, error: "DeniedLogicRef" });
    await assertFixtureUnsettled("batch_groth16.json");
  });

  // A consume-only transaction's one resource is consumed: the check covers
  // consumed resources on their own.
  it("rejects a settlement that only consumes a resource carrying a denied logic ref", () =>
    assertFails(settleUnsettledFixture("batch_groth16_consume_only.json"), { program, error: "DeniedLogicRef" }));

  after(() => funder.drainAll());
});
