/**
 * SPL token forwarder through the adapter: wraps and unwraps settled with
 * fixtures whose external calls target the forwarder, and the committee
 * operations it refuses while the adapter runs. The unwraps spend the wrap's
 * resource through the tree the fresh deployment holds after the wrap, so
 * this file runs in the fresh phase, after the settlements the unwrap
 * fixtures are proven over.
 */
import * as anchor from "@anchor-lang/core";
import {
  AccountMeta,
  AddressLookupTableAccount,
  Ed25519Program,
  PublicKey,
  SystemProgram,
  Keypair,
  PACKET_DATA_SIZE,
  VersionedTransaction,
} from "@solana/web3.js";
import { approve, createMint, getAccount, getOrCreateAssociatedTokenAccount, mintTo } from "@solana/spl-token";
import { assert } from "chai";
import {
  escrowAccounts,
  escrowTransferAccounts,
  setKindTableCommitment,
  forwarderSegmentHead,
  wrapTransferAccounts,
} from "../../client/instructions";
import { NONCES_PER_WORD } from "../../client/constants";
import { deriveNonceBitmapPda, nonceWordIndex } from "../../client/pda";
import { SOLANA_DEVNET_KIND_TABLE_COMMITMENT, UNWRAP_RECIPIENT_SEED_LABEL } from "../utils/constants";
import {
  localCloseAllNonceBitmaps,
  localCloseConfig,
  localCloseEscrow,
  localSetEmergencyCaller,
} from "../utils/localOnly";
import { createdCommitmentsOf as commitmentsOf, requireFixture, wrapAuthorizationIx } from "../utils/fixtures";
import { predictRootAfterAppend } from "../utils/merkle";
import {
  approvedTokenAccount,
  compileV0,
  makeFunder,
  seededKeypair,
  freshUploadId,
  createFundedEscrow,
  assertFails,
  confirmedTransaction,
} from "../utils/helpers";
import {
  provider,
  program,
  forwarderProgram,
  paState,
  testForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureForwarderConfig,
  forwarderCommittee as emergencyCommittee,
  cpiEventsOf,
  settleFromTxDataBuilder,
  useAdapterSuite,
} from "../utils/adapterSuite";

// The fixtures' proofs bind the call inputs, so the tests rebuild the
// fixture's seeded user, mint and recipient and supply the accounts the call
// names.
describe("protocol-adapter (SPL token forwarder wrap and unwrap)", () => {
  const { extendSettlementTable, settleForwarderFixture } = useAdapterSuite();
  // The block's actors hold their SOL across tests, out of the suite's
  // after-each drain.
  const funder = makeFunder(provider);

  const wrapFixture = requireFixture("spl_token_wrap.json");
  // The same wrap terms (user, mint, amount, nonce) under a different
  // nullifier: a replay of the nonce the adapter cannot catch.
  const wrapReplayFixture = requireFixture("spl_token_wrap_replay.json");
  // A second wrap (the next forwarder nonce) proven against the solana-devnet
  // kind table, which lists this mint's transfer kind under the forwarder's label.
  const devnetTableWrapFixture = requireFixture("spl_token_wrap_devnet_kind_table.json");
  // A program the adapter invokes, relaying an unwrap to this forwarder.
  const relayFixture = requireFixture("batch_forwarder_relay.json");
  const wrap = wrapFixture.spl_token_wrap!;

  // The fixture's seeded actors. The user is the mint authority and mints
  // its own supply.
  const user = seededKeypair(wrap.user_seed_label);
  const mintKeypair = seededKeypair(wrap.mint_seed_label);
  const recipient = seededKeypair(UNWRAP_RECIPIENT_SEED_LABEL);
  const mint = mintKeypair.publicKey;
  const { escrowAuthority, escrowAta } = escrowAccounts(forwarderProgram.programId, mint);

  const wrapAmount = BigInt(wrap.amount);
  const wrapNonce = BigInt(wrap.nonce);
  const [nonceBitmapPda, nonceBitmapBump] = deriveNonceBitmapPda(
    forwarderProgram.programId,
    user.publicKey,
    nonceWordIndex(wrapNonce),
  );
  assert.equal(wrapReplayFixture.spl_token_wrap!.nonce, wrap.nonce, "the replay fixture reuses the wrap nonce");

  // The unwrap of the wrap's resource, and the same resource released to the
  // forwarder's own escrow authority: both spend it through its Merkle path
  // in the tree the fresh deployment holds once the wrap settles after the
  // fresh phase's earlier settlements (regen-fixtures.sh proves them over the
  // same leaves).
  const unwraps = {
    unwrap: requireFixture("spl_token_unwrap.json"),
    toEscrow: requireFixture("spl_token_unwrap_to_escrow.json"),
  };
  const unwrapTerms = unwraps.unwrap.spl_token_unwrap!;
  assert.equal(unwrapTerms.recipient_seed_label, UNWRAP_RECIPIENT_SEED_LABEL, "the unwrap pays the seeded recipient");
  assert.equal(unwrapTerms.mint_seed_label, wrap.mint_seed_label, "the unwrap releases the wrapped mint");
  assert.equal(BigInt(unwrapTerms.amount), wrapAmount, "the unwrap releases the wrapped amount");

  let userAta: PublicKey;
  let recipientAta: PublicKey;
  let settlementTable: AddressLookupTableAccount;

  before(async () => {
    const rootAfterWrap = await predictRootAfterAppend(program, paState, commitmentsOf(wrapFixture));
    assert.equal(
      rootAfterWrap.toString("base64"),
      unwraps.unwrap.historical_roots_b64?.[0],
      "the unwrap fixtures were proven over another tree than the one this deployment holds after the wrap; " +
        "the fresh phase's order or fixtures changed: regenerate with scripts/regen-fixtures.sh",
    );

    await funder.fund(user, 5);
    await funder.fund(recipient, 1);

    const config = await ensureForwarderConfig();
    assert.deepEqual(
      config.logicRef,
      Array.from(Buffer.from(wrap.logic_ref_b64, "base64")),
      "the forwarder serves the transfer logic the wrap fixtures are proven with",
    );

    if (!(await provider.connection.getAccountInfo(mint))) {
      await createMint(provider.connection, user, user.publicKey, null, 6, mintKeypair);
    }
    userAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, user.publicKey)).address;
    await mintTo(provider.connection, user, mint, userAta, user, Number(wrapAmount));
    await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, escrowAuthority, true);
    recipientAta = (await getOrCreateAssociatedTokenAccount(provider.connection, recipient, mint, recipient.publicKey))
      .address;
    await approve(provider.connection, user, userAta, escrowAuthority, user, Number(wrapAmount));

    settlementTable = await extendSettlementTable([mint]);
  });

  const segmentHead = forwarderSegmentHead(forwarderProgram.programId);

  const wrapSegment = (destination = escrowAta, source = userAta, nonceBitmap = nonceBitmapPda): AccountMeta[] => [
    ...segmentHead,
    ...wrapTransferAccounts(source, destination, escrowAuthority, nonceBitmap),
  ];

  const unwrapSegment = (destination = recipientAta, source = escrowAta): AccountMeta[] => [
    ...segmentHead,
    ...escrowTransferAccounts(source, destination, escrowAuthority),
  ];

  const relaySegment = (): AccountMeta[] => [
    { pubkey: testForwarderId, isWritable: false, isSigner: false },
    ...segmentHead,
    ...escrowTransferAccounts(escrowAta, recipientAta, escrowAuthority),
  ];

  /**
   * The permissionless instruction that creates `owner`'s bitmap for `word`:
   * by default the user's, for the wrap nonce's word.
   */
  const initNonceBitmapIx = (owner = user.publicKey, word = nonceWordIndex(wrapNonce)) =>
    forwarderProgram.methods
      .initNonceBitmap(owner, new anchor.BN(word.toString()))
      .accountsPartial({
        payer: provider.wallet.publicKey,
        nonceBitmap: deriveNonceBitmapPda(forwarderProgram.programId, owner, word)[0],
        systemProgram: SystemProgram.programId,
      })
      .instruction();

  const balances = (...atas: PublicKey[]) =>
    Promise.all(atas.map((ata) => getAccount(provider.connection, ata).then((a) => a.amount)));

  // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_pa_is_not_stopped
  it("rejects set_emergency_caller while the adapter is running", async () => {
    await funder.fund(emergencyCommittee, 1);
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused, "the adapter must not be paused here");
    await assertFails(
      localSetEmergencyCaller(forwarderProgram, emergencyCommittee.publicKey, paState, Keypair.generate().publicKey)
        .signers([emergencyCommittee])
        .rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotPaused" },
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
    const { uploadId, txData } = freshUploadId(program.programId, authority.publicKey);
    const nullifierAccounts = deriveNullifierAccounts(wrapFixture.consumed_nullifiers_b64);
    const settle = await settleFromTxDataBuilder(authority.publicKey, uploadId, txData, DUMMY_ROOT_MARKER, [
      ...nullifierAccounts,
      ...wrapSegment(),
    ]).transaction();
    const instructions = [
      wrapAuthorizationIx(user.publicKey, wrapFixture),
      await initNonceBitmapIx(),
      ...settle.instructions,
    ];
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
    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(), [wrapAuthorizationIx(user.publicKey, wrapFixture)]),
      {
        program: forwarderProgram,
        error: "NonceBitmapMissing",
      },
    );
  });

  // The destination account is chosen by the submitter, not by the proof.
  it("rejects a wrap whose destination the escrow does not own", async () => {
    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(recipientAta), [
        wrapAuthorizationIx(user.publicKey, wrapFixture),
        await initNonceBitmapIx(),
      ]),
      { program: forwarderProgram, error: "WrongTokenAccountOwner" },
    );
  });

  // The source account is chosen by the submitter, not by the proof. An
  // account whose owner approved the escrow as delegate must not fund a
  // wrap someone else signed: the wrap debits only the signing user.
  it("rejects a wrap whose source the signing user does not own", async () => {
    const otherAta = await approvedTokenAccount(
      provider.connection,
      funder,
      mint,
      user,
      escrowAuthority,
      Number(wrapAmount),
    );
    const before = await balances(otherAta, escrowAta);

    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(escrowAta, otherAta), [
        wrapAuthorizationIx(user.publicKey, wrapFixture),
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
    await approve(provider.connection, user, userOtherAta, escrowAuthority, user, Number(wrapAmount));
    const escrowOwnedOtherAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, user, otherMint, escrowAuthority, true)
    ).address;
    const before = await balances(userOtherAta, escrowOwnedOtherAta, escrowAta);

    await assertFails(
      settleForwarderFixture(wrapFixture, wrapSegment(escrowOwnedOtherAta, userOtherAta), [
        wrapAuthorizationIx(user.publicKey, wrapFixture),
        await initNonceBitmapIx(),
      ]),
      { program: forwarderProgram, error: "WrongTokenAccountMint" },
    );

    assert.deepEqual(await balances(userOtherAta, escrowOwnedOtherAta, escrowAta), before, "no tokens move");
  });

  // The wrap input names the user and the index of the ed25519 instruction
  // that authorizes it; the submitter supplies that instruction. Each of these
  // reaches the authorization check (the bitmap exists, the accounts are the
  // user's and the escrow's) and must stop there with no tokens moved.
  const refusesWrapAuthorization = async (preInstructions: anchor.web3.TransactionInstruction[], error: string) => {
    const before = await balances(userAta, escrowAta);
    await assertFails(settleForwarderFixture(wrapFixture, wrapSegment(), preInstructions), {
      program: forwarderProgram,
      error,
    });
    assert.deepEqual(await balances(userAta, escrowAta), before, "no tokens move");
  };
  const signedWrapMessage = Buffer.from(wrap.signed_message_b64, "base64");

  it("rejects a wrap authorized by another key's signature over the wrap message", async () =>
    refusesWrapAuthorization(
      [
        Ed25519Program.createInstructionWithPrivateKey({
          privateKey: Keypair.generate().secretKey,
          message: signedWrapMessage,
        }),
        await initNonceBitmapIx(),
      ],
      "Ed25519PubkeyMismatch",
    ));

  it("rejects a wrap whose authorization is the user's signature over another message", async () => {
    const otherMessage = Buffer.from(signedWrapMessage);
    otherMessage[0] ^= 1;
    await refusesWrapAuthorization(
      [
        Ed25519Program.createInstructionWithPrivateKey({ privateKey: user.secretKey, message: otherMessage }),
        await initNonceBitmapIx(),
      ],
      "Ed25519MessageMismatch",
    );
  });

  it("rejects a wrap whose authorization is not at the instruction index its input names", async () =>
    refusesWrapAuthorization(
      [await initNonceBitmapIx(), wrapAuthorizationIx(user.publicKey, wrapFixture)],
      "InvalidEd25519Instruction",
    ));

  // Mirrors ERC20Forwarder.t.sol: test_wrap_pulls_funds_from_user. The
  // first wrap on a word carries init_nonce_bitmap in the same transaction,
  // after the ed25519 instruction the wrap input points at (index 0).
  it("settles a wrap: escrow receives the tokens and the nonce is marked used", async () => {
    const [userBefore, escrowBefore] = await balances(userAta, escrowAta);

    const sig = await settleForwarderFixture(wrapFixture, wrapSegment(), [
      wrapAuthorizationIx(user.publicKey, wrapFixture),
      await initNonceBitmapIx(),
    ]);

    const [userAfter, escrowAfter] = await balances(userAta, escrowAta);
    assert.equal(userAfter, userBefore - wrapAmount, "user balance decreases by the wrap amount");
    assert.equal(escrowAfter, escrowBefore + wrapAmount, "escrow holds the wrapped tokens");

    // Mirrors ERC20Forwarder's `Wrapped` event. It travels in the program
    // log, which the runtime truncates past 10,000 bytes per transaction.
    const logs = (await confirmedTransaction(provider.connection, sig)).meta!.logMessages!;
    const wrapped = [
      ...new anchor.EventParser(forwarderProgram.programId, forwarderProgram.coder).parseLogs(logs),
    ].filter((e) => e.name === "wrapped");
    assert.lengthOf(wrapped, 1, "the settlement emits one Wrapped event");
    assert.equal(BigInt(wrapped[0].data.amount.toString()), wrapAmount);

    // Both resources carry the AnomaPay transfer logic the forwarder config
    // pins: the wrap settled under the real verifying key.
    const actionEvents = (await cpiEventsOf(sig)).events.filter((e) => e.name === "actionExecutedEvent");
    assert.equal(actionEvents.length, 1, "one actionExecutedEvent");
    const logicRef = Array.from(Buffer.from(wrap.logic_ref_b64, "base64"));
    const { consumedLogicRefs, createdLogicRefs } = actionEvents[0].data;
    assert.deepEqual(
      [...consumedLogicRefs, ...createdLogicRefs].map((r: number[]) => Array.from(r)),
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
    await approve(provider.connection, user, userAta, escrowAuthority, user, Number(wrapAmount));
    const [escrowBefore] = await balances(escrowAta);
    await assertFails(
      settleForwarderFixture(wrapReplayFixture, wrapSegment(), [
        wrapAuthorizationIx(user.publicKey, wrapReplayFixture),
      ]),
      { program: forwarderProgram, error: "NonceAlreadyUsed" },
    );
    assert.deepEqual(await balances(escrowAta), [escrowBefore], "escrow is unchanged");
  });

  // The submitter chooses the bitmap account too. The user holds and has
  // approved the replay's amount, so if any of these accounts made the used
  // nonce look unused, the replay would move tokens.
  const refusesReplay = async (
    segment: AccountMeta[],
    preInstructions: anchor.web3.TransactionInstruction[],
    expected: { error: string; account?: string },
  ) => {
    await mintTo(provider.connection, user, mint, userAta, user, Number(wrapAmount));
    await approve(provider.connection, user, userAta, escrowAuthority, user, Number(wrapAmount));
    const before = await balances(userAta, escrowAta);
    await assertFails(
      settleForwarderFixture(wrapReplayFixture, segment, [
        wrapAuthorizationIx(user.publicKey, wrapReplayFixture),
        ...preInstructions,
      ]),
      { program: forwarderProgram, ...expected },
    );
    assert.deepEqual(await balances(userAta, escrowAta), before, "no tokens move");
  };

  it("rejects a replay that supplies the user's bitmap of another word", async () => {
    const otherWord = nonceWordIndex(wrapNonce) + 1n;
    await refusesReplay(
      wrapSegment(escrowAta, userAta, deriveNonceBitmapPda(forwarderProgram.programId, user.publicKey, otherWord)[0]),
      [await initNonceBitmapIx(user.publicKey, otherWord)],
      { error: "InvalidNonceBitmapPda" },
    );
  });

  it("rejects a replay that supplies another user's bitmap of the nonce's word", async () => {
    const other = Keypair.generate().publicKey;
    const word = nonceWordIndex(wrapNonce);
    await refusesReplay(
      wrapSegment(escrowAta, userAta, deriveNonceBitmapPda(forwarderProgram.programId, other, word)[0]),
      [await initNonceBitmapIx(other, word)],
      { error: "InvalidNonceBitmapPda" },
    );
  });

  // Not read as an all-zero bitmap in which the nonce is unused.
  it("rejects a wrap whose nonce bitmap is an account of another program", async () =>
    refusesReplay(wrapSegment(escrowAta, userAta, userAta), [], { error: "NonceBitmapMissing" }));

  // The config the forwarder checks the caller and logic ref against is at a
  // fixed address; another account there is refused in account validation.
  it("rejects a forward_call whose config is an account of another program", async () => {
    const [head, , sysvar] = segmentHead;
    const segment = [
      head,
      { pubkey: mint, isSigner: false, isWritable: false },
      sysvar,
      ...wrapTransferAccounts(userAta, escrowAta, escrowAuthority, nonceBitmapPda),
    ];
    await refusesReplay(segment, [], { error: "AccountOwnedByWrongProgram", account: "config" });
  });

  // The recipient account is chosen by the submitter, not by the proof.
  it("rejects an unwrap to a token account the recipient does not own", async () => {
    const { unwrap } = unwraps;
    await assertFails(settleForwarderFixture(unwrap, unwrapSegment(userAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });
  });

  // The escrow authority signs the release; as a delegate it could move any account
  // that approved it. An unwrap pays only from an account the escrow owns.
  it("rejects an unwrap whose source the escrow does not own", async () => {
    const { unwrap } = unwraps;
    const otherAta = await approvedTokenAccount(
      provider.connection,
      funder,
      mint,
      user,
      escrowAuthority,
      Number(wrapAmount),
    );
    const before = await balances(otherAta, recipientAta);

    await assertFails(settleForwarderFixture(unwrap, unwrapSegment(recipientAta, otherAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });

    assert.deepEqual(await balances(otherAta, recipientAta), before, "no tokens move");
  });

  // One escrow authority owns every mint's escrow account, so the owner
  // check alone does not keep an unwrap to its mint: the input names the
  // mint, and the escrow account must hold it.
  it("rejects an unwrap that draws another mint's escrow", async () => {
    const { unwrap } = unwraps;
    const other = await createFundedEscrow(provider, forwarderProgram.programId, user, wrapAmount);
    assert.isTrue(other.escrowAuthority.equals(escrowAuthority), "every mint's escrow has the same authority");
    const recipientOtherAta = (
      await getOrCreateAssociatedTokenAccount(provider.connection, recipient, other.mint, recipient.publicKey)
    ).address;
    const before = await balances(other.escrowAta, recipientOtherAta);

    await assertFails(settleForwarderFixture(unwrap, unwrapSegment(recipientOtherAta, other.escrowAta), []), {
      program: forwarderProgram,
      error: "WrongTokenAccountMint",
    });

    assert.deepEqual(await balances(other.escrowAta, recipientOtherAta), before, "no tokens move");
  });

  // The EVM forwarder reverts an unwrap to itself (BalanceMismatch: its
  // balance does not grow by the amount). Released to the escrow authority,
  // the tokens would never leave custody while the resource is spent.
  it("rejects an unwrap whose recipient is the escrow authority", async () => {
    const { toEscrow } = unwraps;
    const [escrowBefore] = await balances(escrowAta);
    await assertFails(settleForwarderFixture(toEscrow, unwrapSegment(escrowAta), []), {
      program: forwarderProgram,
      error: "UnwrapToEscrow",
    });
    assert.deepEqual(await balances(escrowAta), [escrowBefore], "escrow is unchanged");
  });

  // Mirrors ERC20Forwarder.t.sol: test_unwrap_sends_funds_to_the_user
  it("settles an unwrap: the recipient receives the tokens from escrow", async () => {
    const { unwrap } = unwraps;
    const [escrowBefore, recipientBefore] = await balances(escrowAta, recipientAta);

    await settleForwarderFixture(unwrap, unwrapSegment(), []);

    const [escrowAfter, recipientAfter] = await balances(escrowAta, recipientAta);
    assert.equal(escrowAfter, escrowBefore - wrapAmount, "escrow balance decreases by the unwrap amount");
    assert.equal(recipientAfter, recipientBefore + wrapAmount, "recipient receives the unwrapped tokens");
  });

  // The kind table anoma/risc0-kind-tables generates for solana-devnet is
  // installable: a wrap proven against it is rejected under the commitment
  // the deployment holds and settles once the authority installs the
  // commitment risc0-kind-tables publishes for devnet. The test then
  // restores the commitment it found.
  it("settles a wrap proven against the solana-devnet kind table once the authority installs its commitment", async () => {
    const devnetWrap = devnetTableWrapFixture.spl_token_wrap!;
    const amount = BigInt(devnetWrap.amount);
    assert.deepEqual(
      deriveNonceBitmapPda(forwarderProgram.programId, user.publicKey, nonceWordIndex(BigInt(devnetWrap.nonce)))[0],
      nonceBitmapPda,
      "the nonce shares the main wrap's bitmap word, which the main wrap created",
    );
    await mintTo(provider.connection, user, mint, userAta, user, Number(amount));
    await approve(provider.connection, user, userAta, escrowAuthority, user, Number(amount));
    const settleDevnetWrap = () =>
      settleForwarderFixture(devnetTableWrapFixture, wrapSegment(), [
        wrapAuthorizationIx(user.publicKey, devnetTableWrapFixture),
      ]);

    const found = (await program.account.paStateAccount.fetch(paState)).kindTableCommitment;
    assert.notDeepEqual(
      found,
      Array.from(SOLANA_DEVNET_KIND_TABLE_COMMITMENT),
      "the deployment must hold another table for the wrap to be rejected first",
    );
    await assertFails(settleDevnetWrap(), { program: program, error: "KindTableCommitmentMismatch" });

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
      await setKindTableCommitment(program, provider.wallet.publicKey, found).rpc();
    }
    assert.deepEqual(
      (await program.account.paStateAccount.fetch(paState)).kindTableCommitment,
      found,
      "the commitment the deployment held is restored",
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
  // the adapter is paused. Teardown while running would drain the escrow,
  // forget used nonces or disable the forwarder under live resources.
  it("rejects close_escrow while the adapter is running", async () => {
    const before = await balances(escrowAta, recipientAta);
    await assertFails(
      localCloseEscrow(forwarderProgram, emergencyCommittee.publicKey, paState, { mint, escrowAta, recipientAta })
        .signers([emergencyCommittee])
        .rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotPaused" },
    );
    assert.deepEqual(await balances(escrowAta, recipientAta), before, "no tokens move");
  });

  it("rejects close_nonce_bitmaps_batch while the adapter is running", async () => {
    assert.isNotEmpty(await forwarderProgram.account.nonceBitmap.all(), "the wraps above created nonce bitmaps");
    await assertFails(
      localCloseAllNonceBitmaps(forwarderProgram, emergencyCommittee.publicKey, paState, [emergencyCommittee]),
      { program: forwarderProgram, error: "ProtocolAdapterNotPaused" },
    );
  });

  it("rejects close_config while the adapter is running", () =>
    assertFails(
      localCloseConfig(forwarderProgram, emergencyCommittee.publicKey, paState).signers([emergencyCommittee]).rpc(),
      { program: forwarderProgram, error: "ProtocolAdapterNotPaused" },
    ));

  after(() => funder.drainAll());
});
