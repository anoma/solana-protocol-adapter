/**
 * The adapter upgraded in place across a state-layout change, as pa-evm's
 * UUPS proxy is upgraded: the validator starts on the adapter's previous
 * build (tests/fixtures/previous/protocol_adapter.so, the build deployed on
 * devnet, schema version 1), settles through it, is upgraded to this build
 * (schema version 2, which adds the logic-ref denylist), and its state is
 * migrated in place: the tree, its roots and the configuration carry over.
 */
import { createHash } from "crypto";
import { execFileSync } from "child_process";
import { Keypair, SystemProgram, Transaction, TransactionInstruction } from "@solana/web3.js";
import { assert } from "chai";
import { migrateState, unpauseAdapter } from "../client/instructions";
import { EMPTY_KIND_TABLE_COMMITMENT, SCHEMA_VERSION } from "../client/constants";
import { deriveProgramDataPda, deriveRootMarkerPda } from "../client/pda";
import { loadFixture, createdCommitmentsOf } from "./utils/fixtures";
import { EMPTY_TREE, computeRootAfterAppend } from "./utils/merkle";
import { makeFunder, randomRef, assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  PROOF_SELECTOR,
  buildSettleRemainingAccounts,
  deriveNullifierAccounts,
  useAdapterSuite,
} from "./utils/adapterSuite";
import { VERIFIER_ROUTER_ID } from "../client/verifier";

describe("protocol-adapter (upgraded in place across a state-layout change)", () => {
  const { settleUnsettledFixture, settleFixtureViaTxData } = useAdapterSuite({ initialize: false });
  const funder = makeFunder(provider);
  const authority = provider.wallet.publicKey;

  /** An instruction of the previous build: its Anchor discriminator and Borsh arguments. */
  const previousInstruction = (name: string, args: Buffer[], keys: TransactionInstruction["keys"]) =>
    new TransactionInstruction({
      programId: program.programId,
      keys,
      data: Buffer.concat([createHash("sha256").update(`global:${name}`).digest().subarray(0, 8), ...args]),
    });

  /** The state account's raw bytes: the previous layout is not this build's. */
  const stateBytes = async () => (await provider.connection.getAccountInfo(paState))!.data;

  let stateBeforeUpgrade: Buffer;

  // The previous build's initialize takes the verifier router, the proof
  // selector and the kind-table commitment, over the state, the payer, the
  // system program, the program and its ProgramData.
  it("initializes the previous build", async () => {
    const initializePrevious = previousInstruction(
      "initialize",
      [VERIFIER_ROUTER_ID.toBuffer(), PROOF_SELECTOR, EMPTY_KIND_TABLE_COMMITMENT],
      [
        { pubkey: paState, isSigner: false, isWritable: true },
        { pubkey: authority, isSigner: true, isWritable: true },
        { pubkey: SystemProgram.programId, isSigner: false, isWritable: false },
        { pubkey: program.programId, isSigner: false, isWritable: false },
        { pubkey: deriveProgramDataPda(program.programId), isSigner: false, isWritable: false },
      ],
    );
    await provider.sendAndConfirm(new Transaction().add(initializePrevious));
    assert.equal((await stateBytes())[8], 1, "the previous build writes schema version 1");
  });

  // The previous build's state is not this build's layout, so the root the
  // settlement produces is predicted from the fresh tree rather than read.
  it("settles through the previous build", async () => {
    const fixture = loadFixture("batch_groth16.json");
    const newRoot = computeRootAfterAppend(EMPTY_TREE, createdCommitmentsOf(fixture));
    await settleFixtureViaTxData(
      Buffer.from(fixture.tx_b64, "base64"),
      buildSettleRemainingAccounts(deriveNullifierAccounts(fixture.consumed_nullifiers_b64)),
      { newRootMarker: deriveRootMarkerPda(paState, newRoot, program.programId) },
    );
  });

  // The previous build's two-step transfer: a pending authority that is
  // proposed and then cancelled leaves the serialized state 32 bytes shorter
  // than before, over stale bytes, so the migration must parse the previous
  // layout rather than reinterpret its bytes. Both take the state and the
  // authority.
  it("proposes and cancels an authority transfer through the previous build", async () => {
    const keys = [
      { pubkey: paState, isSigner: false, isWritable: true },
      { pubkey: authority, isSigner: true, isWritable: false },
    ];
    const proposed = Keypair.generate().publicKey;
    await provider.sendAndConfirm(
      new Transaction().add(previousInstruction("propose_authority", [proposed.toBuffer()], keys)),
    );
    await provider.sendAndConfirm(new Transaction().add(previousInstruction("cancel_authority_transfer", [], keys)));
    stateBeforeUpgrade = Buffer.from(await stateBytes());
  });

  // The previous build's one-way stop; its lifecycle byte is this build's
  // paused flag. It takes the state and the authority.
  it("stops the adapter through the previous build", async () => {
    await provider.sendAndConfirm(
      new Transaction().add(
        previousInstruction(
          "emergency_stop",
          [],
          [
            { pubkey: paState, isSigner: false, isWritable: true },
            { pubkey: authority, isSigner: true, isWritable: false },
          ],
        ),
      ),
    );
  });

  it("upgrades the adapter in place to this build", () => {
    execFileSync(
      "solana",
      [
        "program",
        "deploy",
        "target/deploy/protocol_adapter.so",
        "--program-id",
        "target/deploy/protocol_adapter-keypair.json",
        "--keypair",
        process.env.ANCHOR_WALLET!,
        "--url",
        provider.connection.rpcEndpoint,
      ],
      { stdio: "inherit" },
    );
  });

  // Every instruction that reads the state refuses the previous layout. Here
  // the typed load itself fails (AccountDidNotDeserialize, Anchor's 3003)
  // before the schema check: the cancelled transfer left stale bytes where
  // this layout reads the denylist's length. Without them it would be
  // UnsupportedStateSchema; either way nothing misreads the account.
  it("refuses to settle before the state is migrated", () =>
    assertFails(settleUnsettledFixture("batch_groth16_v2.json"), { program: program.programId, code: 3003 }));

  // Only the upgrade authority migrates, as only the EVM proxy's owner calls
  // upgradeToAndCall.
  it("rejects the migration from anyone but the upgrade authority", async () => {
    const intruder = await funder.fresh(1);
    await assertFails(migrateState(program, intruder.publicKey).signers([intruder]).rpc(), {
      program,
      error: "Unauthorized",
    });
  });

  // The operator's command, as run on a cluster after the upgrade.
  const runOperatorScript = (script: string, env: Record<string, string> = {}) =>
    execFileSync("npx", ["ts-node", "-P", "tsconfig.json", script], {
      env: { ...process.env, ...env },
      encoding: "utf-8",
    });

  it("migrates the state in place, keeping the tree, its roots and the configuration", async () => {
    assert.include(runOperatorScript("scripts/migrate-state.ts"), "Migrated PAState");

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.schemaVersion, SCHEMA_VERSION, "the state is in this build's layout");
    assert.deepEqual(state.deniedLogicRefs, [], "no logic ref is denied");
    assert.isTrue(state.paused, "the previous build's stop is a pause");
    assert.ok(state.verifierRouter.equals(VERIFIER_ROUTER_ID), "the verifier router carries over");
    assert.deepEqual(Buffer.from(state.proofSelector), PROOF_SELECTOR, "the proof selector carries over");
    assert.deepEqual(Buffer.from(state.kindTableCommitment), EMPTY_KIND_TABLE_COMMITMENT);
    assert.equal(state.nextIndex.toNumber(), 1, "the tree keeps its leaf");
    // The root's place in the previous layout: discriminator, schema version,
    // bump, authority, router, selector, kind table, an absent pending
    // authority (one byte) and the lifecycle.
    const rootOffset = 8 + 1 + 1 + 32 + 32 + 4 + 32 + 1 + 1;
    assert.deepEqual(
      Buffer.from(state.root),
      stateBeforeUpgrade.subarray(rootOffset, rootOffset + 32),
      "the root carries over",
    );
  });

  it("rejects migrating the state twice", async () => {
    await assertFails(migrateState(program, authority).rpc(), { program, error: "NotPreviousSchema" });
    assert.include(runOperatorScript("scripts/migrate-state.ts"), "already at schema version");
  });

  it("unpauses the migrated adapter, which the previous build could not undo", async () => {
    await unpauseAdapter(program, authority).rpc();
    assert.isFalse((await program.account.paStateAccount.fetch(paState)).paused);
  });

  it("settles and denies logic refs after the migration", async () => {
    await settleUnsettledFixture("batch_groth16_v2.json");
    assert.equal((await program.account.paStateAccount.fetch(paState)).nextIndex.toNumber(), 2);

    const ref = randomRef();
    runOperatorScript("scripts/deny-logic-ref.ts", { PA_DENIED_LOGIC_REF: Buffer.from(ref).toString("hex") });
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(
      state.deniedLogicRefs.map((r: number[]) => Array.from(r)),
      [ref],
    );
  });

  after(() => funder.drainAll());
});
