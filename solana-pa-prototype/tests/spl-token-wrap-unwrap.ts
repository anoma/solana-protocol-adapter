/**
 * SPL token forwarder through the adapter: wraps and unwraps settled with
 * fixtures whose external calls target the forwarder, and the committee
 * operations it refuses while the adapter runs.
 */
import * as anchor from "@anchor-lang/core";
import {
  AccountMeta,
  AddressLookupTableAccount,
  PublicKey,
  SystemProgram,
  Keypair,
  Ed25519Program,
  PACKET_DATA_SIZE,
  SYSVAR_INSTRUCTIONS_PUBKEY,
  VersionedTransaction,
} from "@solana/web3.js";
import {
  approve,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import {
  escrowAccounts,
  escrowTransferAccounts,
  setLogicRef,
  closeAllNonceBitmaps,
  closeConfig,
  closeEscrow,
  setEmergencyCaller,
  setKindTableCommitment,
} from "../client/instructions";
import { EMPTY_KIND_TABLE_COMMITMENT, NONCES_PER_WORD } from "../client/constants";
import { deriveTxDataPda, deriveConfigPda, deriveNonceBitmapPda, nonceWordIndex } from "../client/pda";
import { SOLANA_DEVNET_KIND_TABLE_COMMITMENT } from "./utils/constants";
import { requireFixture, type Fixture, createdCommitmentsOf as commitmentsOf } from "./utils/fixtures";
import {
  randomRef,
  approvedTokenAccount,
  compileV0,
  makeFunder,
  seededKeypair,
  freshUploadId,
  createFundedEscrow,
  assertFails,
} from "./utils/helpers";
import { predictRootAfterAppend } from "./utils/merkle";
import {
  provider,
  program,
  forwarderProgram,
  paState,
  testForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  initForwarderConfig,
  cpiEventsOf,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "./utils/adapterSuite";

// The fixtures' proofs bind the call inputs, so the tests rebuild the
// fixture's seeded user, mint and recipient and supply the accounts the call
// names.
describe("protocol-adapter (SPL token forwarder wrap and unwrap)", () => {
  const { extendSettlementTable, uploadAndSettleV0 } = useAdapterSuite();
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  // The block's actors hold their SOL across tests, out of the suite's
  // after-each drain.
  const funder = makeFunder(provider);

  const wrapFixture = requireFixture("spl_token_wrap.json");
  // The same wrap terms (user, mint, amount, nonce) under a different
  // nullifier: a replay of the nonce the adapter cannot catch.
  const wrapReplayFixture = requireFixture("spl_token_wrap_replay.json");
  const unwrapFixture = requireFixture("spl_token_unwrap.json");
  // A second wrap (forwarder nonce 2) proven against the solana-devnet kind
  // table, which lists this mint's transfer kind under the forwarder's label.
  const devnetTableWrapFixture = requireFixture("spl_token_wrap_devnet_kind_table.json");
  // A program the adapter invokes, relaying an unwrap to this forwarder.
  const relayFixture = requireFixture("batch_forwarder_relay.json");
  const wrap = wrapFixture.spl_token_wrap!;
  const unwrap = unwrapFixture.spl_token_unwrap!;

  // The fixture's seeded actors. The user is the mint authority and mints
  // its own supply.
  const user = seededKeypair(wrap.user_seed_label);
  const mintKeypair = seededKeypair(wrap.mint_seed_label);
  const recipient = seededKeypair(unwrap.recipient_seed_label);
  const mint = mintKeypair.publicKey;
  const { escrowPda, escrowAta } = escrowAccounts(forwarderProgram.programId, mint);
  const emergencyCommittee = Keypair.generate();

  const wrapAmount = BigInt(wrap.amount);
  const wrapNonce = BigInt(wrap.nonce);
  const unwrapAmount = BigInt(unwrap.amount);
  const [nonceBitmapPda, nonceBitmapBump] = deriveNonceBitmapPda(forwarderProgram.programId, user.publicKey, wrapNonce);
  assert.equal(wrapReplayFixture.spl_token_wrap!.nonce, wrap.nonce, "the replay fixture reuses the wrap nonce");
  assert.equal(wrap.mint_seed_label, unwrap.mint_seed_label, "both fixtures must name the same mint");

  let userAta: PublicKey;
  let recipientAta: PublicKey;
  let settlementTable: AddressLookupTableAccount;

  before(async () => {
    await ensureAdapterInitialized();

    // The unwrap fixture spends the resource the wrap creates, through a
    // Merkle path over a fresh adapter's tree holding only the wrap: the
    // wrap must be the first settlement on this adapter.
    const rootAfterWrap = await predictRootAfterAppend(program, paState, commitmentsOf(wrapFixture));
    assert.equal(
      rootAfterWrap.toString("base64"),
      unwrapFixture.historical_roots_b64?.[0],
      "the unwrap fixture was proven over a tree holding only the wrap's commitment, which this adapter does not build; regenerate the SPL fixtures with scripts/regen-fixtures.sh",
    );

    await funder.fund(user, 5);
    await funder.fund(recipient, 1);

    await initForwarderConfig(Array.from(Buffer.from(wrap.logic_ref_b64, "base64")), emergencyCommittee.publicKey);

    await createMint(provider.connection, user, user.publicKey, null, 6, mintKeypair);
    userAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, user.publicKey)).address;
    await mintTo(provider.connection, user, mint, userAta, user, Number(wrapAmount));
    await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, escrowPda, true);
    recipientAta = (await getOrCreateAssociatedTokenAccount(provider.connection, recipient, mint, recipient.publicKey))
      .address;
    await approve(provider.connection, user, userAta, escrowPda, user, Number(wrapAmount));

    settlementTable = await extendSettlementTable([mint]);
  });

  /**
   * Settle `fx` from a TxData upload with the forwarder's account segment
   * after the nullifier markers. `preInstructions` are prepended so an
   * ed25519 instruction lands at index 0, where the wrap input points.
   */
  async function settleForwarderFixture(
    fx: Fixture,
    forwarderAccounts: AccountMeta[],
    preInstructions: anchor.web3.TransactionInstruction[],
  ): Promise<string> {
    const authority = Keypair.generate();
    await funder.fund(authority, 2);
    return uploadAndSettleV0(
      authority,
      Buffer.from(fx.tx_b64, "base64"),
      [...deriveNullifierAccounts(fx.consumed_nullifiers_b64), ...forwarderAccounts],
      { createdCommitments: commitmentsOf(fx) },
      preInstructions,
    );
  }

  const segmentHead: AccountMeta[] = [
    { pubkey: forwarderProgram.programId, isWritable: false, isSigner: false },
    { pubkey: configPda, isWritable: false, isSigner: false },
    { pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isWritable: false, isSigner: false },
  ];

  const wrapSegment = (destination = escrowAta, source = userAta): AccountMeta[] => [
    ...segmentHead,
    { pubkey: source, isWritable: true, isSigner: false },
    { pubkey: destination, isWritable: true, isSigner: false },
    { pubkey: escrowPda, isWritable: false, isSigner: false },
    { pubkey: nonceBitmapPda, isWritable: true, isSigner: false },
    { pubkey: TOKEN_PROGRAM_ID, isWritable: false, isSigner: false },
  ];

  const unwrapSegment = (destination = recipientAta, source = escrowAta): AccountMeta[] => [
    ...segmentHead,
    ...escrowTransferAccounts(source, destination, escrowPda),
  ];

  const relaySegment = (): AccountMeta[] => [
    { pubkey: testForwarderId, isWritable: false, isSigner: false },
    ...segmentHead,
    ...escrowTransferAccounts(escrowAta, recipientAta, escrowPda),
  ];

  /** The ed25519 instruction carrying the fixture's signature over its signed message. */
  const wrapAuthorizationIx = (fx: Fixture) =>
    Ed25519Program.createInstructionWithPublicKey({
      publicKey: user.publicKey.toBytes(),
      message: Buffer.from(fx.spl_token_wrap!.signed_message_b64, "base64"),
      signature: Buffer.from(fx.spl_token_wrap!.signature_b64, "base64"),
    });

  /** The permissionless instruction that creates the user's bitmap for the wrap nonce's word. */
  const initNonceBitmapIx = () =>
    forwarderProgram.methods
      .initNonceBitmap(user.publicKey, new anchor.BN(nonceWordIndex(wrapNonce).toString()))
      .accountsPartial({
        payer: provider.wallet.publicKey,
        nonceBitmap: nonceBitmapPda,
        systemProgram: SystemProgram.programId,
      })
      .instruction();

  const balances = (...atas: PublicKey[]) =>
    Promise.all(atas.map((ata) => getAccount(provider.connection, ata).then((a) => a.amount)));

  // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_pa_is_not_stopped
  it("rejects set_emergency_caller while the adapter is running", async () => {
    await funder.fund(emergencyCommittee, 1);
    const state = await program.account.paStateAccount.fetch(paState);
    assert.deepEqual(state.lifecycle, { running: {} }, "the adapter must be running here");
    await assertFails(
      setEmergencyCaller(forwarderProgram, emergencyCommittee.publicKey, paState, Keypair.generate().publicKey)
        .signers([emergencyCommittee])
        .rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotStopped" },
    );
  });

  // The largest settlement shape in the suite: the ed25519 authorization,
  // the inline bitmap init and the settle with an 8-account wrap segment.
  // Compiled as a v0 message against the deployment's lookup table it must
  // fit one packet. The compiler looks up every table key that is neither a
  // signer nor an invoked program, so the only way a static key survives is
  // by being one of those or by differing per settlement; anything else is a
  // deployment-fixed account missing from the table. Nothing is sent, so the
  // upload account is derived, not created.
  it("compiles the first-wrap settlement as a v0 message within the packet size", async () => {
    const authority = Keypair.generate();
    const { uploadId, uploadIdLe } = freshUploadId();
    const txData = deriveTxDataPda(program.programId, authority.publicKey, uploadIdLe);
    const nullifierAccounts = deriveNullifierAccounts(wrapFixture.consumed_nullifiers_b64);
    const settle = await settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, [
      ...nullifierAccounts,
      ...wrapSegment(),
    ]).transaction();
    const instructions = [wrapAuthorizationIx(wrapFixture), await initNonceBitmapIx(), ...settle.instructions];
    const message = await compileV0(provider, instructions, settlementTable);
    const size = new VersionedTransaction(message).serialize().length;
    const lookedUp = message.addressTableLookups.reduce(
      (n, l) => n + l.writableIndexes.length + l.readonlyIndexes.length,
      0,
    );
    console.log(
      `first-wrap settlement as v0: ${size} bytes, ${message.staticAccountKeys.length} static keys, ${lookedUp} looked up`,
    );
    assert.isAtMost(size, PACKET_DATA_SIZE, "the first-wrap settlement must fit one packet");

    const perSettlement = [
      txData,
      DUMMY_ROOT_MARKER,
      ...nullifierAccounts.map((a) => a.pubkey),
      userAta,
      nonceBitmapPda,
    ];
    const explained = [
      provider.wallet.publicKey,
      authority.publicKey,
      ...instructions.map((ix) => ix.programId),
      ...perSettlement,
    ];
    const unexplained = message.staticAccountKeys.filter((k) => !explained.some((e) => e.equals(k)));
    assert.deepEqual(
      unexplained.map((k) => k.toBase58()),
      [],
      "a static key that is neither a signer, an invoked program nor a per-settlement account belongs in the lookup table",
    );
  });

  // The adapter forwards no signer to the forwarder, so the forwarder cannot
  // create the bitmap during the wrap; a wrap on a word without one fails.
  it("rejects a wrap whose nonce bitmap does not exist", async () => {
    assert.isNull(await provider.connection.getAccountInfo(nonceBitmapPda), "no bitmap yet for this word");
    await assertFails(settleForwarderFixture(wrapFixture, wrapSegment(), [wrapAuthorizationIx(wrapFixture)]), {
      program: forwarderProgram,
      error: "NonceBitmapMissing",
    });
  });

  // Mirrors ForwarderBase.t.sol: test_forwardCall_reverts_if_the_logic_ref_is_wrong,
  // after a rotation. Once the upgrade authority rotates the config to
  // another ref, a wrap proven under the previous one is rejected before
  // any of its input is read; rotating back re-authorizes it.
  it("rejects a wrap proven under a logic ref the config no longer authorizes", async () => {
    const rotate = (ref: number[]) => setLogicRef(forwarderProgram, provider.wallet.publicKey, ref).rpc();
    const fixtureRef = Array.from(Buffer.from(wrap.logic_ref_b64, "base64"));

    await rotate(randomRef());
    try {
      await assertFails(settleForwarderFixture(wrapFixture, wrapSegment(), [wrapAuthorizationIx(wrapFixture)]), {
        program: forwarderProgram,
        error: "UnauthorizedLogicRef",
      });
    } finally {
      await rotate(fixtureRef);
    }
    assert.deepEqual(
      (await forwarderProgram.account.config.fetch(configPda)).logicRef,
      fixtureRef,
      "the fixture's ref is authorized again",
    );
  });

  // The destination account is chosen by the submitter, not by the proof.
  it("rejects a wrap whose destination the escrow does not own", async () => {
    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(recipientAta), [
        wrapAuthorizationIx(wrapFixture),
        await initNonceBitmapIx(),
      ]),
      { program: forwarderProgram, error: "WrongTokenAccountOwner" },
    );
  });

  // The source account is chosen by the submitter, not by the proof. An
  // account whose owner approved the escrow as delegate must not fund a
  // wrap someone else signed: the wrap debits only the signing user.
  it("rejects a wrap whose source the signing user does not own", async () => {
    const otherAta = await approvedTokenAccount(provider.connection, funder, mint, user, escrowPda, Number(wrapAmount));
    const before = await balances(otherAta, escrowAta);

    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(escrowAta, otherAta), [
        wrapAuthorizationIx(wrapFixture),
        await initNonceBitmapIx(),
      ]),
      { program: forwarderProgram, error: "WrongTokenAccountOwner" },
    );

    assert.deepEqual(await balances(otherAta, escrowAta), before, "no tokens move");
  });

  // Every transfer names its mint only through the input; SPL Transfer checks
  // only that source and destination share a mint. A wrap of this mint must
  // not move tokens of another one.
  it("rejects a wrap that moves tokens of another mint", async () => {
    const otherMint = await createMint(provider.connection, user, user.publicKey, null, 6);
    const userOtherAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, otherMint, user.publicKey))
      .address;
    await mintTo(provider.connection, user, otherMint, userOtherAta, user, Number(wrapAmount));
    await approve(provider.connection, user, userOtherAta, escrowPda, user, Number(wrapAmount));
    const escrowOwnedOtherAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, user, otherMint, escrowPda, true)
    ).address;
    const before = await balances(userOtherAta, escrowOwnedOtherAta, escrowAta);

    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(escrowOwnedOtherAta, userOtherAta), [
        wrapAuthorizationIx(wrapFixture),
        await initNonceBitmapIx(),
      ]),
      { program: forwarderProgram, error: "WrongTokenAccountMint" },
    );

    assert.deepEqual(await balances(userOtherAta, escrowOwnedOtherAta, escrowAta), before, "no tokens move");
  });

  // Mirrors ERC20Forwarder.t.sol: test_wrap_pulls_funds_from_user. The
  // first wrap on a word carries init_nonce_bitmap in the same transaction,
  // after the ed25519 instruction the wrap input points at (index 0).
  it("settles a wrap: escrow receives the tokens and the nonce is marked used", async () => {
    const [userBefore, escrowBefore] = await balances(userAta, escrowAta);

    const sig = await settleForwarderFixture(wrapFixture, wrapSegment(), [
      wrapAuthorizationIx(wrapFixture),
      await initNonceBitmapIx(),
    ]);

    const [userAfter, escrowAfter] = await balances(userAta, escrowAta);
    assert.equal(userAfter, userBefore - wrapAmount, "user balance decreases by the wrap amount");
    assert.equal(escrowAfter, escrowBefore + wrapAmount, "escrow holds the wrapped tokens");

    // Both resources carry the AnomaPay transfer logic the forwarder config
    // pins: the wrap settled under the real verifying key.
    const txEvents = (await cpiEventsOf(sig)).events.filter((e) => e.name === "transactionExecutedEvent");
    assert.equal(txEvents.length, 1, "one transactionExecutedEvent");
    const logicRef = Array.from(Buffer.from(wrap.logic_ref_b64, "base64"));
    assert.deepEqual(
      txEvents[0].data.logicRefs.map((r: number[]) => Array.from(r)),
      [logicRef, logicRef],
      "the consumed and created resources carry the AnomaPay transfer logic",
    );

    const bitmap = await forwarderProgram.account.nonceBitmap.fetch(nonceBitmapPda);
    const bit = Number(wrapNonce % NONCES_PER_WORD);
    assert.ok(bitmap.bits[bit >> 3] & (1 << (bit & 7)), "the wrap's nonce is marked used");
    assert.equal(bitmap.bump, nonceBitmapBump, "init_nonce_bitmap stores the canonical bump");
  });

  // Mirrors ERC20Forwarder.t.sol: test_wrap_reverts_if_the_signature_was_already_used
  it("rejects a wrap that replays a used nonce", async () => {
    await approve(provider.connection, user, userAta, escrowPda, user, Number(wrapAmount));
    const [escrowBefore] = await balances(escrowAta);
    await assertFails(
      settleForwarderFixture(wrapReplayFixture, wrapSegment(), [wrapAuthorizationIx(wrapReplayFixture)]),
      { program: forwarderProgram, error: "NonceAlreadyUsed" },
    );
    assert.deepEqual(await balances(escrowAta), [escrowBefore], "escrow is unchanged");
  });

  // The recipient account is chosen by the submitter, not by the proof.
  it("rejects an unwrap to a token account the recipient does not own", async () => {
    await assertFails(settleForwarderFixture(unwrapFixture, unwrapSegment(userAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });
  });

  // The escrow PDA signs the release; as a delegate it could move any account
  // that approved it. An unwrap pays only from an account the escrow owns.
  it("rejects an unwrap whose source the escrow does not own", async () => {
    const otherAta = await approvedTokenAccount(
      provider.connection,
      funder,
      mint,
      user,
      escrowPda,
      Number(unwrapAmount),
    );
    const before = await balances(otherAta, recipientAta);

    await assertFails(settleForwarderFixture(unwrapFixture, unwrapSegment(recipientAta, otherAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });

    assert.deepEqual(await balances(otherAta, recipientAta), before, "no tokens move");
  });

  // One escrow authority owns every mint's escrow account, so the owner
  // check alone does not keep an unwrap to its mint: the input names the
  // mint, and the escrow account must hold it.
  it("rejects an unwrap that draws another mint's escrow", async () => {
    const other = await createFundedEscrow(provider, forwarderProgram.programId, user, unwrapAmount);
    assert.isTrue(other.escrowPda.equals(escrowPda), "every mint's escrow has the same authority");
    const recipientOtherAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, recipient, other.mint, recipient.publicKey)
    ).address;
    const before = await balances(other.escrowAta, recipientOtherAta);

    await assertFails(settleForwarderFixture(unwrapFixture, unwrapSegment(recipientOtherAta, other.escrowAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountMint",
    });

    assert.deepEqual(await balances(other.escrowAta, recipientOtherAta), before, "no tokens move");
  });

  // Mirrors ERC20Forwarder.t.sol: test_unwrap_sends_funds_to_the_user
  it("settles an unwrap: the recipient receives the tokens from escrow", async () => {
    const [escrowBefore, recipientBefore] = await balances(escrowAta, recipientAta);

    await settleForwarderFixture(unwrapFixture, unwrapSegment(), []);

    const [escrowAfter, recipientAfter] = await balances(escrowAta, recipientAta);
    assert.equal(escrowAfter, escrowBefore - unwrapAmount, "escrow balance decreases by the unwrap amount");
    assert.equal(recipientAfter, recipientBefore + unwrapAmount, "recipient receives the unwrapped tokens");
  });

  // The kind table anoma/risc0-kind-tables generates for solana-devnet is
  // installable: a wrap proven against it is rejected under the empty
  // table's commitment and settles once the authority installs the
  // commitment risc0-kind-tables publishes for devnet. Settled after the
  // unwrap, so the unwrap fixture's tree is unchanged.
  it("settles a wrap proven against the solana-devnet kind table once the authority installs its commitment", async () => {
    const devnetWrap = devnetTableWrapFixture.spl_token_wrap!;
    const amount = BigInt(devnetWrap.amount);
    assert.deepEqual(
      deriveNonceBitmapPda(forwarderProgram.programId, user.publicKey, BigInt(devnetWrap.nonce))[0],
      nonceBitmapPda,
      "the nonce shares the main wrap's bitmap word, which the main wrap created",
    );
    await mintTo(provider.connection, user, mint, userAta, user, Number(amount));
    await approve(provider.connection, user, userAta, escrowPda, user, Number(amount));
    const settleDevnetWrap = () =>
      settleForwarderFixture(devnetTableWrapFixture, wrapSegment(), [wrapAuthorizationIx(devnetTableWrapFixture)]);

    await assertFails(settleDevnetWrap(), { program: program, error: "KindTableCommitmentMismatch" });

    const empty = Array.from(EMPTY_KIND_TABLE_COMMITMENT);
    await setKindTableCommitment(
      program,
      provider.wallet.publicKey,
      Array.from(SOLANA_DEVNET_KIND_TABLE_COMMITMENT),
    ).rpc();
    try {
      const [userBefore, escrowBefore] = await balances(userAta, escrowAta);
      await settleDevnetWrap();
      const [userAfter, escrowAfter] = await balances(userAta, escrowAta);
      assert.equal(userAfter, userBefore - amount, "user balance decreases by the wrap amount");
      assert.equal(escrowAfter, escrowBefore + amount, "escrow holds the wrapped tokens");
    } finally {
      await setKindTableCommitment(program, provider.wallet.publicKey, empty).rpc();
    }
    assert.deepEqual(
      (await program.account.paStateAccount.fetch(paState)).kindTableCommitment,
      empty,
      "the empty table's commitment is restored",
    );
  });

  // The adapter invokes another program during a settlement, and that program
  // calls this forwarder. Only the adapter itself may call it.
  it("rejects a forward_call relayed by a program the adapter invokes", async () => {
    const before = await balances(escrowAta, recipientAta);
    assert.isTrue(before[0] > 0n, "the escrow holds tokens the relay names");

    await assertFails(settleForwarderFixture(relayFixture, relaySegment(), []), {
      program: forwarderProgram,
      error: "UnauthorizedCaller",
    });

    assert.deepEqual(await balances(escrowAta, recipientAta), before, "no tokens move");
  });

  // Mirrors EmergencyMigratableForwarderBase: the committee acts only once
  // the adapter is stopped. Teardown while running would drain the escrow,
  // forget used nonces or disable the forwarder under live resources.
  it("rejects close_escrow while the adapter is running", async () => {
    const before = await balances(escrowAta, recipientAta);
    await assertFails(
      closeEscrow(forwarderProgram, emergencyCommittee.publicKey, paState, { mint, escrowPda, escrowAta, recipientAta })
        .signers([emergencyCommittee])
        .rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotStopped" },
    );
    assert.deepEqual(await balances(escrowAta, recipientAta), before, "no tokens move");
  });

  it("rejects close_nonce_bitmaps_batch while the adapter is running", async () => {
    assert.isNotEmpty(await forwarderProgram.account.nonceBitmap.all(), "the wraps above created nonce bitmaps");
    await assertFails(
      closeAllNonceBitmaps(forwarderProgram, emergencyCommittee.publicKey, paState, [emergencyCommittee]),
      { program: forwarderProgram, error: "ProtocolAdapterNotStopped" },
    );
  });

  it("rejects close_config while the adapter is running", () =>
    assertFails(
      closeConfig(forwarderProgram, emergencyCommittee.publicKey, paState).signers([emergencyCommittee]).rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotStopped" },
    ));

  after(() => funder.drainAll());
});
