/**
 * The adapter upgraded in place across a state-layout change, as pa-evm's
 * UUPS proxy is upgraded: its validator starts on the build deployed on
 * devnet (tests/fixtures/previous/protocol_adapter.so, state schema 2, whose
 * owner is the program's upgrade authority), which settles, denies a logic
 * ref, retunes its expiry bounds and pauses. The program is upgraded through
 * the loader to this build, and `migrate_state` brings the state to schema
 * 3, owned by the upgrade authority, and hands the upgrade authority to the
 * program's PDA, after which the owner alone runs it and upgrades it.
 */
import { BN, Idl, Program } from "@anchor-lang/core";
import { PublicKey, SystemProgram } from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import { PREVIOUS_SCHEMA_VERSION, SCHEMA_VERSION } from "../../client/constants";
import { denyLogicRef, setKindTableCommitment, unpauseAdapter, upgradeAdapter } from "../../client/instructions";
import { deriveUpgradeAuthorityPda } from "../../client/pda";
import { deployedExecutableHash, executableHash, upgradeAuthority } from "../../client/upgrade";
import { VERIFIER_ROUTER_ID, getVerifierEntryPda } from "../../client/verifier";
import { createdCommitmentsOf, loadFixture } from "../utils/fixtures";
import {
  assertFails,
  randomRef,
  sendV0,
  solanaCli,
  uploadTxData,
  waitForSlotPast,
  writeBuffer,
} from "../utils/helpers";
import { localMigrateState } from "../utils/localOnly";
import { predictRootMarkerPda } from "../utils/merkle";
import {
  PROOF_SELECTOR,
  buildSettleRemainingAccounts,
  cpiEventsOf,
  deriveNullifierAccounts,
  paState,
  program,
  provider,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "../utils/adapterSuite";

const ADAPTER_SO = "target/deploy/protocol_adapter.so";

/** The previous build, through its own production IDL (fetched from devnet with it). */
const previous: Program<any> = new Program(
  JSON.parse(readFileSync("tests/fixtures/previous/protocol_adapter.json", "utf8")) as Idl,
  provider,
);

describe("protocol-adapter (upgraded in place from schema 2)", () => {
  const { funder, settlementTable, settleUnsettledFixture } = useAdapterSuite({ initialize: false });
  const wallet = provider.wallet.publicKey;
  const deniedBefore = randomRef();
  let stateBefore: any;

  it("initializes the previous build", async () => {
    await previous.methods
      .initialize(VERIFIER_ROUTER_ID, Array.from(PROOF_SELECTOR))
      .accountsPartial({
        paState,
        payer: wallet,
        systemProgram: SystemProgram.programId,
        verifierEntry: getVerifierEntryPda(PROOF_SELECTOR, VERIFIER_ROUTER_ID)[0],
      })
      .rpc();
    const bytes = (await provider.connection.getAccountInfo(paState))!.data;
    assert.equal(bytes[8], PREVIOUS_SCHEMA_VERSION, "the previous build writes the previous schema version");
  });

  // The previous build settles, denies, retunes and pauses, so the state
  // carries a grown tree, a denylist and non-default bounds into the
  // migration.
  it("builds history through the previous build", async () => {
    const fixture = await loadFixture("batch_groth16.json");
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
      buildSettleRemainingAccounts(deriveNullifierAccounts(fixture.consumed_nullifiers_b64)),
      true,
      previous,
    ).transaction();
    await sendV0(provider, settle.instructions, [authority], await settlementTable());
    if (await provider.connection.getAccountInfo(txData)) {
      await previous.methods
        .txdataClose(uploadId)
        .accountsPartial({ txData, authority: authority.publicKey, refund: authority.publicKey })
        .signers([authority])
        .rpc();
    }

    await previous.methods.denyLogicRef(deniedBefore).accountsPartial({ paState, authority: wallet }).rpc();
    await previous.methods
      .updateExpiryConfig(new BN(150), new BN(200_000))
      .accountsPartial({ paState, authority: wallet })
      .rpc();
    await previous.methods.pause().accountsPartial({ paState, authority: wallet }).rpc();
    stateBefore = await (previous.account as any).paStateAccount.fetch(paState);
    assert.equal(stateBefore.nextIndex.toNumber(), createdCommitmentsOf(fixture).length, "the settlement appended");
  });

  // The loader's new code runs from the slot after the upgrade.
  it("upgrades the program in place to this build through the loader", async () => {
    solanaCli(provider, "program", "deploy", ADAPTER_SO, "--program-id", "target/deploy/protocol_adapter-keypair.json");
    await waitForSlotPast(provider.connection, await provider.connection.getSlot("confirmed"));
  });

  // Every instruction that reads the state refuses the previous layout. Here
  // the typed load itself fails (AccountDidNotDeserialize): the previous
  // layout lacks the owner's 32 bytes, so it does not read as this one, and
  // nothing misreads the account.
  it("refuses an owner-only instruction before the state is migrated", () =>
    assertFails(setKindTableCommitment(program, wallet, Array.from(stateBefore.kindTableCommitment)).rpc(), {
      program,
      error: "AccountDidNotDeserialize",
      account: "pa_state",
    }));

  // Only the upgrade authority migrates, as only the EVM proxy's owner calls
  // upgradeToAndCall.
  it("rejects the migration from anyone but the upgrade authority", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(localMigrateState(program, stranger.publicKey).signers([stranger]).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
  });

  it("migrates the state in place, owned by the upgrade authority, and hands the upgrade authority to the program", async () => {
    const sig = await localMigrateState(program, wallet).rpc();
    const { events } = await cpiEventsOf(sig);
    assert.deepEqual(
      events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
      [["ownershipTransferredEvent", PublicKey.default.toBase58(), wallet.toBase58()]],
      "the migration announces the owner, as initialize does",
    );

    const state: any = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.schemaVersion, SCHEMA_VERSION, "the state is in this build's layout");
    assert.equal(state.owner.toBase58(), wallet.toBase58(), "the upgrade authority is the stored owner");
    for (const field of [
      "bump",
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
      "deniedLogicRefs",
    ]) {
      assert.equal(JSON.stringify(state[field]), JSON.stringify(stateBefore[field]), `${field} carries over`);
    }
    assert.equal(
      (await upgradeAuthority(provider.connection, program.programId))?.toBase58(),
      deriveUpgradeAuthorityPda(program.programId).toBase58(),
      "the program's upgrade authority is its PDA",
    );
  });

  // The upgrade authority is the program's PDA now, so the loader refuses
  // the wallet: the migration cannot run twice, and the loader path is shut.
  it("rejects migrating twice", () =>
    assertFails(localMigrateState(program, wallet).rpc(), { program, error: "Unauthorized", account: "program_data" }));

  it("runs under its owner: unpauses, settles and denies", async () => {
    await unpauseAdapter(program, wallet).rpc();
    await settleUnsettledFixture("batch_groth16_v2.json");
    const deniedAfter = randomRef();
    await denyLogicRef(program, wallet, deniedAfter).rpc();
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused);
    assert.deepEqual(
      state.deniedLogicRefs.map((r: number[]) => Array.from(r)),
      [deniedBefore, deniedAfter],
      "the denylist keeps the previous build's entry",
    );
  });

  it("upgrades through the program from then on", async () => {
    const buffer = writeBuffer(provider, ADAPTER_SO);
    const sig = await upgradeAdapter(program, wallet, buffer, wallet).rpc();
    const { tx, events } = await cpiEventsOf(sig);
    const expected = executableHash(readFileSync(ADAPTER_SO));
    assert.deepEqual(
      events.map((e) => [e.name, Buffer.from(e.data.executableHash).toString("hex")]),
      [["upgradedEvent", expected.toString("hex")]],
    );
    assert.deepEqual(await deployedExecutableHash(provider.connection, program.programId), expected);
    await waitForSlotPast(provider.connection, tx.slot);
    await setKindTableCommitment(program, wallet, Array.from(stateBefore.kindTableCommitment)).rpc();
  });
});
