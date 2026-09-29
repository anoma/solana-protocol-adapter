/**
 * Settlement on an initialized adapter: the primary fixture's settlement,
 * the re-initialization guard, and the rejections that fire before any
 * nullifier is consumed.
 */
import { PublicKey, SystemProgram, Keypair, ComputeBudgetProgram, SYSVAR_CLOCK_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { VERIFIER_ROUTER_ID } from "../scripts/verifier-utils";
import {
  EMPTY_TREE_ROOT_INITIAL,
  loadFixture,
  errorHaystack,
  createdCommitmentsOf as commitmentsOf,
} from "./utils";
import {
  provider,
  program,
  paState,
  fixture,
  VERIFIER,
  VERIFIER_PROGRAM_ID,
  routerPda,
  verifierEntryPda,
  blockTimeForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  buildInitialize,
  ensureAdapterInitialized,
  buildSettleRemainingAccounts,
  PA_ERROR_NAMES,
  extractPAErrorCode,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("settlement", () => {
  const { funder, uploadTxData, uploadAndSettleV0, settleFixtureViaTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  describe("protocol-adapter (Groth16 batch aggregation E2E)", () => {
    const tx = Buffer.from(fixture.tx_b64, "base64");
    const txTampered = Buffer.from(fixture.tx_tampered_b64, "base64");

    const remainingAccounts = deriveNullifierAccounts(fixture.consumed_nullifiers_b64);
    const nullifierPdas = remainingAccounts.map((a) => a.pubkey);

    async function settleViaTxData(
      authority: Keypair,
      payload: Buffer,
      options?: {
        nullifierAccounts?: { pubkey: PublicKey; isWritable: boolean; isSigner: boolean }[];
        newRootMarker?: PublicKey;
        createdCommitments?: Buffer[];
        additionalHistoricalRootMarkers?: PublicKey[];
      }
    ) {
      await funder.fund(authority, 2);
      return uploadAndSettleV0(
        authority,
        payload,
        buildSettleRemainingAccounts(options?.nullifierAccounts ?? remainingAccounts, options),
        options
      );
    }


    it("initializes with depth 1 (variable-depth tree)", async () => {
      const state = await program.account.paStateAccount.fetch(paState);
      assert.equal(
        state.schemaVersion,
        1,
        "a freshly initialized adapter carries schema version 1 (PAStateAccount::SCHEMA_VERSION)"
      );
      assert.isAtLeast(
        state.currentDepth,
        1,
        "Tree depth should be at least 1"
      );
      assert.equal(
        state.frontier.length,
        state.currentDepth,
        "Frontier length should equal current depth"
      );
      if (state.nextIndex.toNumber() === 0) {
        // Fresh PA: root should be genesis
        const rootBytes = Buffer.from(state.root as number[]);
        assert.deepEqual(
          rootBytes,
          EMPTY_TREE_ROOT_INITIAL,
          "Initial root should be ZEROS[0] for depth-1 tree"
        );
      }
    });

    it("account size matches expected size for current depth (no over-allocation)", async () => {
      // Space formula: BASE_SPACE (201) + VEC_OVERHEAD (4) + 32 * depth
      // BASE_SPACE breakdown matches state.rs: discriminator(8) +
      // schema_version(1) + bump(1) + authority(32) + verifier_router(32) +
      // proof_selector(4) + kind_table_commitment(32) + pending_authority(33) +
      // lifecycle(1) + root(32) + next_index(8) + current_depth(1) +
      // expiry bounds(16)
      const BASE_SPACE = 201;
      const VEC_OVERHEAD = 4;
      const spaceForDepth = (depth: number) => BASE_SPACE + VEC_OVERHEAD + 32 * depth;

      const state = await program.account.paStateAccount.fetch(paState);
      const accountInfo = await provider.connection.getAccountInfo(paState);

      assert.ok(accountInfo, "PAState account should exist");

      const expectedSize = spaceForDepth(state.currentDepth);
      assert.equal(
        accountInfo!.data.length,
        expectedSize,
        `Account size (${accountInfo!.data.length}) should match expected size for depth ${state.currentDepth} (${expectedSize})`
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

      try {
        await settleViaTxData(Keypair.generate(), txWitness, { newRootMarker: DUMMY_ROOT_MARKER });
        assert.fail("expected settle to fail");
      } catch (e: any) {
        assertPAError(e, "ExpectedDeltaProof");
      }
    });

    it("rejects a tampered tx (proof binding)", async () => {
      try {
        await settleViaTxData(Keypair.generate(), txTampered, { newRootMarker: DUMMY_ROOT_MARKER });
        assert.fail("expected settle to fail");
      } catch (e: any) {
        // The PA calls the verifier router via CPI, which calls the verifier the
        // fixture's selector routes to. Solana's CPI error propagation records
        // the INNER program's error code in the PA's failure line — so we see
        // the verifier's code instead of the PA's VerifierRouterFailed (6013).
        const code = extractPAErrorCode(e);
        assert.isNotNull(code, "Expected a program error code in logs");
        assert.equal(
          code,
          VERIFIER.rejectionCode,
          `the fixture selector's verifier rejection code (${VERIFIER.rejectionCode}) should propagate through CPI`,
        );
      }
    });

    it("accepts a valid Groth16 batch aggregation tx and creates root marker", async () => {
      // Guardrail: fixture should actually include a block-time-forwarder external call.
      // If not present, this test can pass without exercising the external call path.
      assert.ok(
        tx.includes(Buffer.from(blockTimeForwarderId.toBytes())),
        "fixture tx must include block-time-forwarder program id bytes (external_payload injected)"
      );

      // Requires a fresh ledger: the assertions below pin an exact state
      // transition, which a prior settlement would invalidate.
      const firstNullifier = await provider.connection.getAccountInfo(nullifierPdas[0]);
      assert.isNull(
        firstNullifier,
        "batch_groth16.json is already settled on this validator (its first " +
        "nullifier marker exists). These tests require a fresh ledger. Reset it " +
        "with './scripts/dev.sh clean' and re-run, or deploy to a fresh devnet."
      );

      // Get the current state before settlement to know the pre-settlement root
      const stateBefore = await program.account.paStateAccount.fetch(paState);
      const rootBeforeBytes = Buffer.from(stateBefore.root as number[]);
      const nextIndexBefore = stateBefore.nextIndex.toNumber();

      await settleViaTxData(Keypair.generate(), tx, { createdCommitments: commitmentsOf(fixture) });

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
      assert.notDeepEqual(
        rootAfterBytes,
        rootBeforeBytes,
        "Root should change after appending commitment"
      );
    });

    it("reverts on unexpected forwarder call output (ExternalCallOutputMismatch)", async () => {
      // This test uses a fixture where:
      // - timestamp = -1 (past time, forwarder will return RESULT_LT = 0x00)
      // - expected_output = 0x02 (RESULT_GT - intentionally WRONG)
      // The PA should revert with ExternalCallOutputMismatch when actual != expected.
      const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
      const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

      const mismatchNullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);

      try {
        await settleViaTxData(Keypair.generate(), mismatchTx, {
          nullifierAccounts: mismatchNullifierAccounts,
          newRootMarker: DUMMY_ROOT_MARKER,
        });
        assert.fail("expected settle to fail with ExternalCallOutputMismatch");
      } catch (e: any) {
        assertPAError(e, "ExternalCallOutputMismatch");
      }
    });
  });

  describe("protocol-adapter (Re-initialization guard)", () => {
    it("rejects re-initialization of PAState", async () => {
      // The file's before hook initialized PAState.
      // A second initialize call must fail because the account already exists.
      try {
        await buildInitialize(provider.wallet.publicKey).rpc();
        assert.fail("expected re-initialization to fail");
      } catch (e: any) {
        const haystack = errorHaystack(e);
        // Anchor's init constraint rejects when the account already exists
        assert.match(
          haystack,
          /already in use|already been initialized|0x0/i,
          `Expected 'already in use' error, got: ${haystack}`
        );
      }
    });
  });

  describe("protocol-adapter (Settle error paths)", () => {
    it("rejects wrong verifier_router_program address", async () => {
      const payer = Keypair.generate();
      await funder.fund(payer, 2);

      const fakeRouter = Keypair.generate().publicKey;

      try {
        await program.methods
          .settle(Buffer.from([0, 1, 2, 3]))
          .accountsPartial({
            paState,
            payer: payer.publicKey,
            systemProgram: SystemProgram.programId,
            newRootMarker: DUMMY_ROOT_MARKER,
            verifierRouterProgram: fakeRouter,
            router: routerPda,
            verifierEntry: verifierEntryPda,
            verifierProgram: VERIFIER_PROGRAM_ID,
          })
          .preInstructions([
            ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
          ])
          .signers([payer])
          .rpc();
        assert.fail("expected wrong verifier_router_program to fail");
      } catch (e: any) {
        assertPAError(e, "VerifierRouterFailed");
      }
    });

    it("rejects insufficient remaining_accounts for nullifiers", async () => {
      // Upload the fixture but pass zero nullifier accounts.
      // The program expects 1 nullifier PDA in remaining_accounts.
      const authority = Keypair.generate();
      await funder.fund(authority, 2);

      const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
      const mismatchTx = Buffer.from(mismatchFixture.tx_b64, "base64");

      const { uploadId, txData } = await uploadTxData(authority, mismatchTx);

      // Pass ZERO nullifier accounts but still include forwarder+clock
      const allRemainingAccounts = buildSettleRemainingAccounts([]);

      try {
        await program.methods
          .settleFromTxdata(uploadId)
          .accountsPartial({
            paState,
            txData,
            authority: authority.publicKey,
            systemProgram: SystemProgram.programId,
            newRootMarker: DUMMY_ROOT_MARKER,
            verifierRouterProgram: VERIFIER_ROUTER_ID,
            router: routerPda,
            verifierEntry: verifierEntryPda,
            verifierProgram: VERIFIER_PROGRAM_ID,
          })
          .remainingAccounts(allRemainingAccounts)
          .preInstructions([
            ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
            ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
          ])
          .signers([authority])
          .rpc();
        assert.fail("expected insufficient remaining_accounts to fail");
      } catch (e: any) {
        // With zero nullifier accounts, the program consumes what would be
        // the forwarder/clock slots as nullifier PDAs, then can't find the
        // forwarder in the remaining accounts. The exact error depends on
        // which check fails first.
        const code = extractPAErrorCode(e);
        const name = code !== null ? PA_ERROR_NAMES.get(code) : null;
        const validErrors = ["NullifierPdaMismatch", "UnregisteredForwarder", "InvalidTransactionData"];
        assert.isNotNull(code, "Expected a PA error code");
        assert.include(
          validErrors,
          name,
          `Expected one of ${validErrors.join("|")}, got ${name} (${code})`,
        );
      }
    });

    it("rejects unregistered forwarder program in remaining_accounts", async () => {
      // Pass correct nullifier PDAs but replace the forwarder program ID with
      // a random pubkey. The program searches external_accounts (everything
      // after the nullifier slots) for the forwarder and fails with
      // UnregisteredForwarder when it can't find it.
      const authority = Keypair.generate();
      await funder.fund(authority, 2);

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

      try {
        await program.methods
          .settleFromTxdata(uploadId)
          .accountsPartial({
            paState,
            txData,
            authority: authority.publicKey,
            systemProgram: SystemProgram.programId,
            newRootMarker: DUMMY_ROOT_MARKER,
            verifierRouterProgram: VERIFIER_ROUTER_ID,
            router: routerPda,
            verifierEntry: verifierEntryPda,
            verifierProgram: VERIFIER_PROGRAM_ID,
          })
          .remainingAccounts(remainingAccounts)
          .preInstructions([
            ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
            ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 }),
          ])
          .signers([authority])
          .rpc();
        assert.fail("expected unregistered forwarder to fail");
      } catch (e: any) {
        assertPAError(e, "UnregisteredForwarder");
      }
    });
  });

  describe("protocol-adapter (Settlement error paths — fixture variants)", () => {
    async function expectSettleError(fixtureName: string, expectedError: string) {
      const fx = loadFixture(fixtureName);
      const payload = Buffer.from(fx.tx_b64, "base64");
      const nullifierAccounts = deriveNullifierAccounts(fx.consumed_nullifiers_b64);
      const remainingAccounts = buildSettleRemainingAccounts(nullifierAccounts);

      try {
        await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
        assert.fail(`expected ${expectedError} error`);
      } catch (e: any) {
        assertPAError(e, expectedError);
      }
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
