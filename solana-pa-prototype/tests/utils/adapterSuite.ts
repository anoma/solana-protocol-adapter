/**
 * The protocol adapter as the spec files drive it: program handles, the
 * settlement builders, precondition helpers, and
 * `useAdapterSuite`, the hooks a spec file installs inside its own top-level
 * describe. Only spec files import this module: it resolves the workspace
 * programs when loaded.
 */
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import {
  AccountMeta,
  AddressLookupTableAccount,
  ComputeBudgetProgram,
  Keypair,
  LAMPORTS_PER_SOL,
  PublicKey,
  SystemProgram,
  SYSVAR_CLOCK_PUBKEY,
} from "@solana/web3.js";
import { assert } from "chai";
import { BlockTimeForwarder } from "../../target/types/block_time_forwarder";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { SplTokenForwarder } from "../../target/types/spl_token_forwarder";
import { TestForwarder } from "../../target/types/test_forwarder";
import { MockVerifier } from "../../target/types/mock_verifier";
import { emergencyStop, initializeAdapter, initializeForwarder } from "../../client/instructions";
import { ensureSettlementLookupTable, settlementLookupKeys } from "../../client/lookupTable";
import {
  deriveNullifierAccounts as deriveNullifierAccountsFromB64,
  derivePaStatePda,
  deriveRootMarkerPda,
} from "../../client/pda";
import {
  getRouterPda,
  getVerifierEntryPda,
  GROTH16_VERIFIER_ID,
  MOCK_SELECTOR,
  VERIFIER_ROUTER_ID,
} from "../../client/verifier";
import { EMPTY_TREE_ROOT_INITIAL } from "./constants";
import { createdCommitmentsOf, loadFixture, parseSelectorFromFixture } from "./fixtures";
import {
  confirmedTransaction,
  ExpectedFailure,
  initTxData as initTxDataOf,
  makeFunder,
  sendV0,
  TxDataUpload,
  uploadTxData as uploadTxDataTo,
} from "./helpers";
import { predictRootMarkerPda as predictRootMarkerPdaOf } from "./merkle";

export const provider = anchor.AnchorProvider.env();
anchor.setProvider(provider);

export const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
export const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
export const [paState] = derivePaStatePda(program.programId);

/** The primary fixture: one ephemeral consumed resource, one created, a block-time-forwarder call. */
export const fixture = loadFixture("batch_groth16.json");

// Verifiers by router selector. Fixtures carry their selector, so tests
// derive the verifier program and its failures from the fixture instead of
// hardcoding one. `rejection` rejects a well-formed proof that does not
// verify; `malformedProof` rejects proof bytes that are not valid curve
// points. Unknown selectors fail loudly rather than silently defaulting to
// some verifier.
type Verifier = { program: PublicKey; rejection: ExpectedFailure; malformedProof: ExpectedFailure };

function verifierOf(program: PublicKey, rejectionCode: number, malformedProofCode: number): Verifier {
  return {
    program,
    rejection: { program, code: rejectionCode },
    malformedProof: { program, code: malformedProofCode },
  };
}

function verifierForSelector(selector: Buffer): Verifier {
  const hex = selector.toString("hex");
  switch (hex) {
    // groth_16_verifier: VerificationError, PairingError
    case "73c457ba":
      return verifierOf(GROTH16_VERIFIER_ID, 6000, 6003);
    // mock-verifier (programs/mock-verifier, localnet only): ClaimDigestMismatch
    // for both (it checks no curve points; offset 6600 keeps it disjoint)
    case MOCK_SELECTOR.toString("hex"):
      return verifierOf((anchor.workspace.MockVerifier as Program<MockVerifier>).programId, 6600, 6600);
    default:
      throw new Error(`no verifier registered for selector 0x${hex}`);
  }
}

export const PROOF_SELECTOR = parseSelectorFromFixture(fixture.selector);
export const VERIFIER = verifierForSelector(PROOF_SELECTOR);
export const [routerPda] = getRouterPda(VERIFIER_ROUTER_ID);
export const [verifierEntryPda] = getVerifierEntryPda(PROOF_SELECTOR, VERIFIER_ROUTER_ID);

export const blockTimeForwarderProgram = anchor.workspace.BlockTimeForwarder as Program<BlockTimeForwarder>;
export const testForwarderProgram = anchor.workspace.TestForwarder as Program<TestForwarder>;
export const blockTimeForwarderId = blockTimeForwarderProgram.programId;
export const testForwarderId = testForwarderProgram.programId;

export function deriveRootPda(root: Buffer): PublicKey {
  return deriveRootMarkerPda(paState, root, program.programId);
}

// `newRootMarker` is a required named account, so every settle/settleFromTxdata
// call needs a value even when the test expects settlement to fail before the
// account is ever read. `initialize` never creates a root marker for the
// empty-tree root — `is_root_valid` accepts it directly, without a marker —
// so this PDA never exists on-chain. It's used purely as a syntactically
// valid placeholder address; its value is irrelevant for those tests since
// the instruction fails earlier.
export const DUMMY_ROOT_MARKER = deriveRootPda(EMPTY_TREE_ROOT_INITIAL);

export function predictRootMarkerPda(createdCommitments: Buffer[]): Promise<PublicKey> {
  return predictRootMarkerPdaOf(program, paState, createdCommitments);
}

export function deriveNullifierAccounts(nullifierB64s: string[]): AccountMeta[] {
  return deriveNullifierAccountsFromB64(nullifierB64s, paState, program.programId);
}

// The one set of initialize arguments every spec file deploys with: the
// verifier router and the fixture's selector. Callers add `.signers()` when
// the payer is not the provider wallet.
export const buildInitialize = (payer: PublicKey) =>
  initializeAdapter(program, payer, VERIFIER_ROUTER_ID, Array.from(PROOF_SELECTOR));

export async function paStateExists(): Promise<boolean> {
  return (await provider.connection.getAccountInfo(paState)) !== null;
}

/**
 * Initialize the adapter unless it already is. A fresh validator never has
 * it; a cluster run finds the deployment's own adapter and uses it as is.
 */
export async function ensureAdapterInitialized(): Promise<void> {
  if (!(await paStateExists())) {
    await buildInitialize(provider.wallet.publicKey).rpc();
  }
}

/** Stop the adapter as its authority, the provider wallet. */
export async function stopAdapter(): Promise<void> {
  await emergencyStop(program, provider.wallet.publicKey).rpc();
}

/** Set the TxData expiry bounds as the adapter authority. */
export async function setExpiryBounds(minSlots: number, maxSlots: number): Promise<void> {
  await program.methods
    .updateExpiryConfig(new anchor.BN(minSlots), new anchor.BN(maxSlots))
    .accountsPartial({ paState, authority: provider.wallet.publicKey })
    .rpc();
}

/** Initialize the SPL token forwarder's config for this adapter, the provider wallet paying. */
export async function initForwarderConfig(logicRef: number[], committee: PublicKey): Promise<void> {
  await initializeForwarder(forwarderProgram, program.programId, logicRef, committee, provider.wallet.publicKey).rpc();
}

/**
 * Assert that `fixtureName` has not already been settled on this validator.
 *
 * Every test that settles a fixture asserts an exact state transition
 * (next_index before -> after, a specific resulting root, specific markers).
 * Those assertions are only meaningful when the fixture's nullifiers are
 * unconsumed. If they already exist, the rest of the test is measuring
 * something else.
 *
 * This fails loudly rather than skipping or degrading to a weaker check: a
 * silently retired test reports as pending, which reads as green, and a
 * weakened one reports as passing while verifying materially less.
 */
export async function assertFixtureUnsettled(fixtureName: string): Promise<void> {
  assert.isFalse(
    await fixtureSettled(fixtureName),
    `${fixtureName} is already settled on this validator (its first nullifier ` +
      `marker exists). These tests require a fresh ledger: run them through ` +
      `'./scripts/dev.sh anchor-test', which gives every spec file its own, or deploy to a fresh devnet.`,
  );
}

/** Whether `fixtureName` is settled on this validator: its first nullifier marker exists. */
export async function fixtureSettled(fixtureName: string): Promise<boolean> {
  const [first] = deriveNullifierAccounts(loadFixture(fixtureName).consumed_nullifiers_b64);
  return (await provider.connection.getAccountInfo(first.pubkey)) !== null;
}

/** The fixture's nullifier markers followed by the block-time forwarder's call segment. */
export function buildSettleRemainingAccounts(nullifierAccounts: AccountMeta[]): AccountMeta[] {
  return [
    ...nullifierAccounts,
    { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
    { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
  ];
}

/** Anchor's CPI event tag: the fixed 8-byte `EVENT_IX_TAG_LE`, the little-endian encoding of the u64 0x1d9acb512ea545e4. */
const EVENT_IX_TAG_LE = Buffer.from([0xe4, 0x45, 0xa5, 0x2e, 0x51, 0xcb, 0x9a, 0x1d]);

/**
 * Settlement events are CPI events: inner instructions of the adapter whose
 * data is Anchor's event tag followed by the event's discriminator and Borsh
 * body. Read them in emission order from the confirmed transaction.
 */
function parseCpiEvents(tx: anchor.web3.VersionedTransactionResponse) {
  const keys = tx.transaction.message.getAccountKeys({
    accountKeysFromLookups: tx.meta?.loadedAddresses,
  });
  const coder = new anchor.BorshCoder(program.idl);
  const events: { name: string; data: any }[] = [];
  for (const group of tx.meta?.innerInstructions ?? []) {
    for (const ix of group.instructions) {
      if (!keys.get(ix.programIdIndex)?.equals(program.programId)) continue;
      const data = Buffer.from(anchor.utils.bytes.bs58.decode(ix.data));
      if (data.length < 16 || !data.subarray(0, 8).equals(EVENT_IX_TAG_LE)) continue;
      const decoded = coder.events.decode(data.subarray(8).toString("base64"));
      if (decoded) events.push(decoded);
    }
  }
  return events;
}

/** The settlement's CPI events, once the transaction is confirmed. */
export async function cpiEventsOf(sig: string) {
  const tx = await confirmedTransaction(provider.connection, sig);
  return { tx, events: parseCpiEvents(tx) };
}

/** The full CU budget and, unless `heapFrame` is false, the 256 KiB heap frame settlement needs. */
function settleBudget(heapFrame: boolean) {
  return [
    ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
    ...(heapFrame ? [ComputeBudgetProgram.requestHeapFrame({ bytes: 256 * 1024 })] : []),
  ];
}

/**
 * `settle_from_txdata` with the full CU budget and, unless `heapFrame` is
 * false, the 256 KiB heap frame settlement needs.
 */
export function settleFromTxDataBuilder(
  authority: PublicKey,
  uploadId: anchor.BN,
  txData: PublicKey,
  newRootMarker: PublicKey,
  remainingAccounts: AccountMeta[],
  heapFrame = true,
) {
  return program.methods
    .settleFromTxdata(uploadId)
    .accountsPartial({
      paState,
      txData,
      authority,
      systemProgram: SystemProgram.programId,
      newRootMarker,
      verifierRouterProgram: VERIFIER_ROUTER_ID,
      router: routerPda,
      verifierEntry: verifierEntryPda,
      verifierProgram: VERIFIER.program,
    })
    .remainingAccounts(remainingAccounts)
    .preInstructions(settleBudget(heapFrame));
}

/**
 * `settle` of the inline `payload`, paid by `payer`, with the full CU budget
 * and, unless `heapFrame` is false, the 256 KiB heap frame. Its produced-root
 * marker is the dummy one: every inline settlement the suite sends is one
 * expected to be rejected.
 */
export function settleBuilder(
  payer: PublicKey,
  payload: Buffer,
  remainingAccounts: AccountMeta[] = [],
  heapFrame = true,
) {
  return program.methods
    .settle(payload)
    .accountsPartial({
      paState,
      payer,
      systemProgram: SystemProgram.programId,
      newRootMarker: DUMMY_ROOT_MARKER,
      verifierRouterProgram: VERIFIER_ROUTER_ID,
      router: routerPda,
      verifierEntry: verifierEntryPda,
      verifierProgram: VERIFIER.program,
    })
    .remainingAccounts(remainingAccounts)
    .preInstructions(settleBudget(heapFrame));
}

type TxDataEntry = { uploadId: anchor.BN; txData: PublicKey; authority: Keypair };

/**
 * Install the adapter suite's hooks in the calling describe block and return
 * the file's handles. Call it once, first thing inside a spec file's
 * top-level describe. Before the file's tests it initializes the adapter
 * unless `initialize` is false (for a file that needs it uninitialized).
 * After each test it closes the TxData uploads the test opened (recovering
 * their rent) and drains the keypairs it funded back to the provider wallet,
 * so the same SOL circulates across a cluster run.
 */
export function useAdapterSuite(options: { initialize?: boolean } = {}) {
  const funder = makeFunder(provider);
  // TxData accounts this file's tests opened, closed after each test.
  const openTxData: TxDataEntry[] = [];
  let table: AddressLookupTableAccount | undefined;
  let suiteStartBalance = 0;
  let testStartBalance = 0;

  /**
   * The deployment's settlement lookup table, created on first use from the
   * deployment's fixed keys: settlements are v0 transactions against it, the
   * shape every submitter sends.
   */
  async function settlementTable(): Promise<AddressLookupTableAccount> {
    if (!table) table = await extendSettlementTable([]);
    return table;
  }

  /** Add `mints`' escrow token accounts to the settlement table: they are fixed for the deployment once the mint is supported. */
  async function extendSettlementTable(mints: PublicKey[]): Promise<AddressLookupTableAccount> {
    ({ table } = await ensureSettlementLookupTable(
      provider.connection,
      (provider.wallet as anchor.Wallet).payer,
      settlementLookupKeys({
        paProgram: program.programId,
        verifierRouter: VERIFIER_ROUTER_ID,
        proofSelector: PROOF_SELECTOR,
        verifierProgram: VERIFIER.program,
        blockTimeForwarder: blockTimeForwarderId,
        splTokenForwarder: forwarderProgram.programId,
        mints,
      }),
      table?.key,
    ));
    return table;
  }

  async function uploadTxData(
    authority: Keypair,
    payload: Buffer,
    expiresSlotOverride?: anchor.BN,
  ): Promise<TxDataUpload> {
    const upload = await uploadTxDataTo(program, paState, authority, payload, expiresSlotOverride);
    openTxData.push({ uploadId: upload.uploadId, txData: upload.txData, authority });
    return upload;
  }

  async function initTxData(
    authority: Keypair,
    payloadSize: number,
    expiresSlotOverride?: anchor.BN,
  ): Promise<TxDataUpload> {
    const init = await initTxDataOf(program, paState, authority, payloadSize, expiresSlotOverride);
    openTxData.push({ uploadId: init.uploadId, txData: init.txData, authority });
    return init;
  }

  /**
   * Close `entry`'s TxData, refunding its authority. A successful settlement
   * or the test itself may already have closed it; that is checked, not
   * assumed, and any other failure to close fails the test.
   */
  async function closeTxData(entry: TxDataEntry): Promise<void> {
    if (!(await provider.connection.getAccountInfo(entry.txData))) return;
    await program.methods
      .txdataClose(entry.uploadId)
      .accountsPartial({
        txData: entry.txData,
        authority: entry.authority.publicKey,
        refund: entry.authority.publicKey,
      })
      .signers([entry.authority])
      .rpc();
  }

  /** Stop closing `txData` after each test: the caller keeps it open across tests and closes it itself. */
  function keepTxData(txData: PublicKey): TxDataEntry {
    const i = openTxData.findIndex((e) => e.txData.equals(txData));
    assert.notEqual(i, -1, `TxData ${txData.toBase58()} was not opened through this suite`);
    return openTxData.splice(i, 1)[0];
  }

  /**
   * Upload `payload` under `authority` and settle it as a v0 transaction
   * against the deployment's lookup table, the shape every submitter sends.
   * `preInstructions` are prepended to the settlement's own. The produced-root
   * marker is `newRootMarker`, or predicted from `createdCommitments`.
   */
  async function uploadAndSettleV0(
    authority: Keypair,
    payload: Buffer,
    remainingAccounts: AccountMeta[],
    options?: { newRootMarker?: PublicKey; createdCommitments?: Buffer[] },
    preInstructions: anchor.web3.TransactionInstruction[] = [],
  ): Promise<string> {
    const { uploadId, txData } = await uploadTxData(authority, payload);
    // A settlement expected to succeed must predict its produced-root marker
    // from the transaction's created commitments; one expected to fail passes
    // an explicit (dummy) marker instead.
    let newRootMarker = options?.newRootMarker;
    if (newRootMarker === undefined) {
      assert.ok(
        options?.createdCommitments,
        "settlement without an explicit newRootMarker needs createdCommitments to predict it",
      );
      newRootMarker = await predictRootMarkerPda(options.createdCommitments);
    }
    const settle = await settleFromTxDataBuilder(
      authority.publicKey,
      uploadId,
      txData,
      newRootMarker,
      remainingAccounts,
    ).transaction();
    return sendV0(provider, [...preInstructions, ...settle.instructions], [authority], await settlementTable());
  }

  async function settleFixtureViaTxData(
    payload: Buffer,
    remainingAccounts: AccountMeta[],
    options?: { newRootMarker?: PublicKey; createdCommitments?: Buffer[] },
  ): Promise<string> {
    return uploadAndSettleV0(await funder.fresh(2), payload, remainingAccounts, options);
  }

  /**
   * Settle the fixture `fixtureName`, whose one external call is the
   * block-time forwarder's, asserting first that it is unsettled. Returns the
   * settlement's signature.
   */
  async function settleUnsettledFixture(fixtureName: string): Promise<string> {
    await assertFixtureUnsettled(fixtureName);
    const f = loadFixture(fixtureName);
    return settleFixtureViaTxData(
      Buffer.from(f.tx_b64, "base64"),
      buildSettleRemainingAccounts(deriveNullifierAccounts(f.consumed_nullifiers_b64)),
      { createdCommitments: createdCommitmentsOf(f) },
    );
  }

  /**
   * Settle the fixture `fixtureName` unless it is already settled. A fresh
   * validator never has it; a cluster run shares one deployment across spec
   * files, so another file may have settled it already.
   */
  async function settleFixture(fixtureName: string): Promise<void> {
    if (!(await fixtureSettled(fixtureName))) await settleUnsettledFixture(fixtureName);
  }

  before(async () => {
    suiteStartBalance = await provider.connection.getBalance(provider.wallet.publicKey);
    console.log(`  [sol] suite start: wallet ${(suiteStartBalance / LAMPORTS_PER_SOL).toFixed(4)} SOL`);
  });

  if (options.initialize !== false) before(ensureAdapterInitialized);

  beforeEach(async () => {
    testStartBalance = await provider.connection.getBalance(provider.wallet.publicKey);
  });

  afterEach(async () => {
    for (const entry of openTxData.splice(0)) {
      await closeTxData(entry);
    }
    const { recovered, drained } = await funder.drainAll();

    const balance = await provider.connection.getBalance(provider.wallet.publicKey);
    console.log(
      `    [sol] spent ${((testStartBalance - balance) / LAMPORTS_PER_SOL).toFixed(4)}, ` +
        `recovered ${(recovered / LAMPORTS_PER_SOL).toFixed(4)} ` +
        `(${drained} keypairs), ` +
        `wallet ${(balance / LAMPORTS_PER_SOL).toFixed(4)} SOL`,
    );
  });

  // End-of-file audit: exact breakdown of where SOL went.
  after(async () => {
    const suiteEndBalance = await provider.connection.getBalance(provider.wallet.publicKey);
    const totalSpent = suiteStartBalance - suiteEndBalance;

    // The adapter owns PAState, 0-byte nullifier and root markers, and
    // TxData uploads (every other account with data).
    const allAccounts = await provider.connection.getProgramAccounts(program.programId);
    let paStateRent = 0;
    let markerCount = 0;
    let markerRent = 0;
    let txdataCount = 0;
    let txdataRent = 0;
    for (const { pubkey, account } of allAccounts) {
      if (pubkey.equals(paState)) {
        paStateRent += account.lamports;
      } else if (account.data.length === 0) {
        markerCount++;
        markerRent += account.lamports;
      } else {
        txdataCount++;
        txdataRent += account.lamports;
      }
    }
    const accountRent = paStateRent + markerRent + txdataRent;
    const txFees = totalSpent - accountRent;
    const sol = (lamports: number) => (lamports / LAMPORTS_PER_SOL).toFixed(6);
    const markerLamports = markerCount > 0 ? allAccounts.find((a) => a.account.data.length === 0)!.account.lamports : 0;

    console.log(`\n  [sol] ═══ Suite SOL Audit ═══`);
    console.log(`  [sol]   start balance:     ${sol(suiteStartBalance)} SOL`);
    console.log(`  [sol]   end balance:       ${sol(suiteEndBalance)} SOL`);
    console.log(`  [sol]   total spent:       ${sol(totalSpent)} SOL`);
    console.log(`  [sol]   breakdown:`);
    console.log(`  [sol]     PAState rent:    ${sol(paStateRent)} SOL`);
    console.log(
      `  [sol]     marker rent:     ${sol(markerRent)} SOL (${markerCount} nullifier/root markers × ${markerLamports} lamports each)`,
    );
    console.log(`  [sol]     unclosed TxData: ${sol(txdataRent)} SOL (${txdataCount} accounts)`);
    console.log(`  [sol]     tx fees + dust:  ${sol(txFees)} SOL`);
  });

  return {
    funder,
    extendSettlementTable,
    uploadTxData,
    initTxData,
    closeTxData,
    keepTxData,
    uploadAndSettleV0,
    settleFixtureViaTxData,
    settleUnsettledFixture,
    settleFixture,
  };
}
