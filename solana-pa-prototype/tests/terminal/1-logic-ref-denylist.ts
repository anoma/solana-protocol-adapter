/**
 * The authority's logic-ref denylist, as pa-evm's denyLogicRef: a denied
 * logic ref stays denied, and no settlement consumes or creates a resource
 * carrying it. A denial cannot be undone, and these tests deny the logic
 * almost every fixture carries, so this runs among the suite's last files.
 */
import { assert } from "chai";
import { denyLogicRef } from "../../client/instructions";
import { requireFixture } from "../utils/fixtures";
import { makeFunder, randomRef, assertFails } from "../utils/helpers";
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

describe("protocol-adapter (logic-ref denylist)", () => {
  const { settleUnsettledFixture, settleFixtureViaTxData } = useAdapterSuite();
  const funder = makeFunder(provider);
  const authority = provider.wallet.publicKey;
  const denied = async () =>
    (await program.account.paStateAccount.fetch(paState)).deniedLogicRefs.map((r: number[]) => Array.from(r));

  /** Submit `fixtureName` for settlement, whatever its nullifiers' state. */
  const submit = (fixtureName: string) => {
    const f = requireFixture(fixtureName);
    return settleFixtureViaTxData(
      Buffer.from(f.tx_b64, "base64"),
      buildSettleRemainingAccounts(deriveNullifierAccounts(f.consumed_nullifiers_b64)),
      { newRootMarker: DUMMY_ROOT_MARKER },
    );
  };

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
    const before = await denied();
    const ref = randomRef();
    const sig = await denyLogicRef(program, authority, ref).rpc();

    assert.deepEqual(await denied(), [...before, ref], "the state appends the denied logic ref");
    const events = (await cpiEventsOf(sig)).events.filter((e) => e.name === "logicRefDeniedEvent");
    assert.lengthOf(events, 1, "one LogicRefDenied event");
    assert.deepEqual(Array.from(events[0].data.logicRef), ref);
  });

  it("rejects denying a logic ref twice", async () => {
    const [ref] = await denied();
    await assertFails(denyLogicRef(program, authority, ref).rpc(), {
      program,
      error: "LogicRefAlreadyDenied",
    });
  });

  // pa-evm checks every consumed and created resource: a transaction whose
  // resources carry a denied logic ref does not settle. Every batch
  // fixture's resources carry the passthrough logic; this file's own
  // settlement reports it. The denial is checked before any nullifier is
  // recorded, so the rejected fixture stays unspent.
  it("rejects a settlement whose resources carry a denied logic ref", async () => {
    const sig = await settleUnsettledFixture("batch_groth16_denylist.json");
    const [action] = (await cpiEventsOf(sig)).events.filter((e) => e.name === "actionExecutedEvent");
    const passthrough: number[] = Array.from(action.data.consumedLogicRefs[0]);

    await denyLogicRef(program, authority, passthrough).rpc();
    await assertFails(submit("batch_groth16_rejected.json"), { program, error: "DeniedLogicRef" });
    await assertFixtureUnsettled("batch_groth16_rejected.json");
  });

  // A consume-only transaction's one resource is consumed: the check covers
  // consumed resources on their own.
  it("rejects a settlement that only consumes a resource carrying a denied logic ref", () =>
    assertFails(submit("batch_groth16_consume_only.json"), { program, error: "DeniedLogicRef" }));

  after(() => funder.drainAll());
});
