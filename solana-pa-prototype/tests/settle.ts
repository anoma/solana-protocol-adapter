/**
 * Settlement on an initialized adapter: the primary fixture's settlement,
 * the re-initialization guard, and the rejections that fire before any
 * nullifier is consumed.
 */
import { AccountMeta, PublicKey, SystemProgram, Keypair, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { SCHEMA_VERSION } from "../client/constants";
import { EMPTY_TREE_ROOT_INITIAL } from "./utils/constants";
import { loadFixture, createdCommitmentsOf as commitmentsOf, tamperedTxOf } from "./utils/fixtures";
import { assertFails } from "./utils/helpers";
import {
  provider,
  program,
  paState,
  fixture,
  VERIFIER,
  blockTimeForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  buildInitialize,
  assertFixtureUnsettled,
  buildSettleRemainingAccounts,
  settleBuilder,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("settlement", () => {
  const { funder, uploadTxData, uploadAndSettleV0, settleFixtureViaTxData } = useAdapterSuite();

  describe("protocol-adapter (Groth16 batch aggregation E2E)", () => {
    const tx = Buffer.from(fixture.tx_b64, "base64");
    const txTampered = tamperedTxOf(fixture);

    const remainingAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const nullifierPdas = remainingAccounts.map((a) => a.pubkey);

    async function settleViaTxData(
      payload: Buffer,
      options?: {
        nullifierAccounts?: AccountMeta[];
        newRootMarker?: PublicKey;
        createdCommitments?: Buffer[];
      },
    ) {
      return uploadAndSettleV0(
        await funder.fresh(2),
        payload,
        buildSettleRemainingAccounts(options?.nullifierAccounts ?? remainingAccounts),
        options,
      );
    }

    it("initializes with depth 1 (variable-depth tree)", async () => {
      const state = await program.account.paStateAccount.fetch(paState);
      assert.equal(state.schemaVersion, SCHEMA_VERSION, "a freshly initialized adapter carries SCHEMA_VERSION");
      assert.isAtLeast(state.currentDepth, 1, "Tree depth should be at least 1");
      assert.equal(state.frontier.length, state.currentDepth, "Frontier length should equal current depth");
      if (state.nextIndex.toNumber() === 0) {
        // Fresh PA: root should be genesis
        const rootBytes = Buffer.from(state.root as number[]);
        assert.deepEqual(rootBytes, EMPTY_TREE_ROOT_INITIAL, "Initial root should be ZEROS[0] for depth-1 tree");
      }
    });

    it("account size matches expected size for current depth (no over-allocation)", async () => {
      // The account holds the current frontier and the denylist, and nothing
      // more.
      const state = await program.account.paStateAccount.fetch(paState);
      const accountInfo = await provider.connection.getAccountInfo(paState);

      assert.ok(accountInfo, "PAState account should exist");

      const expectedSize = (await program.coder.accounts.encode("paStateAccount", state)).length;
      assert.equal(
        accountInfo!.data.length,
        expectedSize,
        `Account size (${accountInfo!.data.length}) should match expected size for depth ${state.currentDepth} (${expectedSize})`,
      );
    });

    it("rejects Delta::Witness (balance conservation bypass attempt)", async () => {
      // A WELL-FORMED transaction carrying Delta::Witness — the fixture's
      // aggregated transaction with its delta proof replaced by the actual
      // delta witness (fixture-gen's witness_delta.json error variant). It
      // deserializes cleanly, so the rejection must come from the PA's own
      // witness check — a clean ExpectedDeltaProof, never a crash. A witness
      // scalar is prover-side private data; deserializing it on-chain must
      // never execute curve arithmetic (the k256 stack-overflow class).
      const fx = loadFixture("witness_delta.json");
      const txWitness = Buffer.from(fx.tx_b64, "base64");

      await assertFails(settleViaTxData(txWitness, { newRootMarker: DUMMY_ROOT_MARKER }), {
        program,
        error: "ExpectedDeltaProof",
      });
    });

    it("rejects a tampered tx (proof binding)", async () => {
      // The adapter calls the verifier router, which calls the verifier the
      // fixture's selector routes to: that verifier rejects the proof.
      await assertFails(settleViaTxData(txTampered, { newRootMarker: DUMMY_ROOT_MARKER }), VERIFIER.rejection);
    });

    it("accepts a valid Groth16 batch aggregation tx and creates root marker", async () => {
      // Guardrail: fixture should actually include a block-time-forwarder external call.
      // If not present, this test can pass without exercising the external call path.
      assert.ok(
        tx.includes(Buffer.from(blockTimeForwarderId.toBytes())),
        "fixture tx must include block-time-forwarder program id bytes (external_payload injected)",
      );

      // Requires a fresh ledger: the assertions below pin an exact state
      // transition, which a prior settlement would invalidate.
      await assertFixtureUnsettled("batch_groth16.json");

      // Get the current state before settlement to know the pre-settlement root
      const stateBefore = await program.account.paStateAccount.fetch(paState);
      const rootBeforeBytes = Buffer.from(stateBefore.root as number[]);
      const nextIndexBefore = stateBefore.nextIndex.toNumber();

      await settleViaTxData(tx, { createdCommitments: commitmentsOf(fixture) });

      // Verify nullifier PDAs exist
      for (const pda of nullifierPdas) {
        const info = await provider.connection.getAccountInfo(pda);
        assert.ok(info, "nullifier marker PDA should exist");
        assert.ok(info!.owner.equals(program.programId), "nullifier marker PDA should be owned by PA program");
      }

      const stateAfter = await program.account.paStateAccount.fetch(paState);
      assert.equal(stateAfter.nextIndex.toNumber(), nextIndexBefore + 1);

      // Verify the root changed after settlement
      const rootAfterBytes = Buffer.from(stateAfter.root as number[]);
      assert.notDeepEqual(rootAfterBytes, rootBeforeBytes, "Root should change after appending commitment");
    });

    it("reverts on unexpected forwarder call output (ExternalCallOutputMismatch)", async () => {
      // This test uses a fixture where:
      // - timestamp = -1 (past time, forwarder will return RESULT_LT = 0x00)
      // - expected_output = 0x02 (RESULT_GT - intentionally WRONG)
      // The PA should revert with ExternalCallOutputMismatch when actual != expected.
      const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
      const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

      const mismatchNullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);

      await assertFails(
        settleViaTxData(mismatchTx, {
          nullifierAccounts: mismatchNullifierAccounts,
          newRootMarker: DUMMY_ROOT_MARKER,
        }),
        { program, error: "ExternalCallOutputMismatch" },
      );
    });
  });

  describe("protocol-adapter (Re-initialization guard)", () => {
    it("rejects re-initialization of PAState", async () => {
      // The suite initialized PAState before the file's tests.
      // A second initialize call must fail because the account already exists.
      await assertFails(buildInitialize(provider.wallet.publicKey).rpc(), {
        program: SystemProgram.programId,
        code: 0,
      });
    });
  });

  describe("protocol-adapter (Settle error paths)", () => {
    it("rejects wrong verifier_router_program address", async () => {
      const payer = await funder.fresh(2);

      const fakeRouter = Keypair.generate().publicKey;

      await assertFails(
        settleBuilder(payer.publicKey, Buffer.from([0, 1, 2, 3]), [], false)
          .accountsPartial({ verifierRouterProgram: fakeRouter })
          .signers([payer])
          .rpc(),
        { program, error: "VerifierRouterFailed" },
      );
    });

    it("rejects insufficient remaining_accounts for nullifiers", async () => {
      // Upload the fixture but pass zero nullifier accounts.
      // The program expects 1 nullifier PDA in remaining_accounts.
      const authority = await funder.fresh(2);

      const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
      const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

      const { uploadId, txData } = await uploadTxData(authority, mismatchTx);

      // Pass ZERO nullifier accounts but still include forwarder+clock: the
      // fixture's one nullifier claims the forwarder slot, leaving too few
      // accounts for the forwarder's call segment, a malformed settlement.
      const allRemainingAccounts = buildSettleRemainingAccounts([]);

      await assertFails(
        settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, allRemainingAccounts)
          .signers([authority])
          .rpc(),
        { program, error: "InvalidTransactionData" },
      );
    });

    it("rejects unregistered forwarder program in remaining_accounts", async () => {
      // Pass correct nullifier PDAs but replace the forwarder program ID with
      // a random pubkey. The program searches external_accounts (everything
      // after the nullifier slots) for the forwarder and fails with
      // UnregisteredForwarder when it can't find it.
      const authority = await funder.fresh(2);

      const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
      const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

      const { uploadId, txData } = await uploadTxData(authority, mismatchTx);

      const nullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);

      // Build remaining_accounts manually with a FAKE forwarder instead of
      // the real blockTimeForwarderId
      const fakeForwarder = Keypair.generate().publicKey;
      const remainingAccounts = [
        ...nullifierAccounts,
        { pubkey: fakeForwarder, isWritable: false, isSigner: false },
        { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
      ];

      await assertFails(
        settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, remainingAccounts)
          .signers([authority])
          .rpc(),
        { program, error: "UnregisteredForwarder" },
      );
    });
  });

  describe("protocol-adapter (Settlement error paths — fixture variants)", () => {
    async function expectSettleError(fixtureName: string, expectedError: string) {
      const fx = loadFixture(fixtureName);
      const payload = Buffer.from(fx.tx_b64, "base64");
      const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
      const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

      await assertFails(settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER }), {
        program,
        error: expectedError,
      });
    }

    it("rejects NonExistingRoot (wrong commitment tree root)", async () => {
      await expectSettleError("wrong_root.json", "NonExistingRoot");
    });

    it("rejects AggregationRequired (no aggregation proof)", async () => {
      await expectSettleError("no_aggregation.json", "AggregationRequired");
    });

    it("rejects InvalidProof (garbage aggregation proof bytes)", async () => {
      await expectSettleError("garbage_proof.json", "InvalidProof");
    });
  });
});
