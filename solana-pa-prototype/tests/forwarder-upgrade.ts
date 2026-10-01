/**
 * The SPL token forwarder upgraded in place, as the EVM forwarder's proxy is
 * upgraded: the validator starts on the forwarder's previous build
 * (tests/fixtures/previous/spl_token_forwarder.so, the build deployed on
 * devnet), a wrap settles through it, the program is upgraded to this build,
 * and the custody and replay protection the previous build recorded keep
 * working.
 */
import * as anchor from "@anchor-lang/core";
import { execFileSync } from "child_process";
import { AccountMeta, PublicKey, SystemProgram } from "@solana/web3.js";
import { approve, createMint, getAccount, getOrCreateAssociatedTokenAccount, mintTo } from "@solana/spl-token";
import { assert } from "chai";
import {
  escrowAccounts,
  escrowTransferAccounts,
  migrateConfig,
  migrateEscrow,
  migrateNonceBitmap,
  previousEscrowAccounts,
  forwarderSegmentHead,
  wrapTransferAccounts,
} from "../client/instructions";
import { NONCES_PER_WORD, PREVIOUS_NONCE_BITMAP_SIZE } from "../client/constants";
import { deriveConfigPda, deriveNonceBitmapPda, nonceWordIndex } from "../client/pda";
import { requireFixture, wrapAuthorizationIx } from "./utils/fixtures";
import { makeFunder, seededKeypair, assertFails } from "./utils/helpers";
import { provider, forwarderProgram, initForwarderConfig, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (SPL token forwarder upgraded in place)", () => {
  const { extendSettlementTable, settleForwarderFixture } = useAdapterSuite();
  const funder = makeFunder(provider);
  const forwarderId = forwarderProgram.programId;
  const [configPda] = deriveConfigPda(forwarderId);

  const wrapFixture = requireFixture("spl_token_wrap.json");
  const wrapReplayFixture = requireFixture("spl_token_wrap_replay.json");
  const unwrapFixture = requireFixture("spl_token_unwrap.json");
  const wrap = wrapFixture.spl_token_wrap!;
  const unwrap = unwrapFixture.spl_token_unwrap!;

  const user = seededKeypair(wrap.user_seed_label);
  const mintKeypair = seededKeypair(wrap.mint_seed_label);
  const recipient = seededKeypair(unwrap.recipient_seed_label);
  const mint = mintKeypair.publicKey;
  const wrapAmount = BigInt(wrap.amount);
  const wrapNonce = BigInt(wrap.nonce);
  const unwrapAmount = BigInt(unwrap.amount);
  const [nonceBitmapPda, nonceBitmapBump] = deriveNonceBitmapPda(
    forwarderId,
    user.publicKey,
    nonceWordIndex(wrapNonce),
  );

  // The previous build held each mint's escrow under its own authority,
  // ["escrow", mint]; this build holds every mint's under ["escrow"].
  const { previousEscrowAuthority, previousEscrowAta } = previousEscrowAccounts(forwarderId, mint);
  const { escrowAuthority, escrowAta } = escrowAccounts(forwarderId, mint);

  let userAta: PublicKey;
  let recipientAta: PublicKey;

  const balance = async (ata: PublicKey) => (await getAccount(provider.connection, ata)).amount;

  const segmentHead = forwarderSegmentHead(forwarderId);

  const wrapSegment = (authority: PublicKey, destination: PublicKey): AccountMeta[] => [
    ...segmentHead,
    ...wrapTransferAccounts(userAta, destination, authority, nonceBitmapPda),
  ];

  before(async () => {
    await funder.fund(user, 5);
    await funder.fund(recipient, 1);
    await createMint(provider.connection, user, user.publicKey, null, 6, mintKeypair);
    userAta = (await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, user.publicKey)).address;
    await mintTo(provider.connection, user, mint, userAta, user, Number(wrapAmount));
    recipientAta = (await getOrCreateAssociatedTokenAccount(provider.connection, recipient, mint, recipient.publicKey))
      .address;
    await extendSettlementTable([mint]);
  });

  // The previous build's own instructions create its accounts: the config,
  // the user's nonce bitmap, and the per-mint escrow a wrap pays into.
  it("settles a wrap through the previous build", async () => {
    await initForwarderConfig(Array.from(Buffer.from(wrap.logic_ref_b64, "base64")), provider.wallet.publicKey);
    await getOrCreateAssociatedTokenAccount(provider.connection, user, mint, previousEscrowAuthority, true);
    await approve(provider.connection, user, userAta, previousEscrowAuthority, user, Number(wrapAmount));
    await forwarderProgram.methods
      .initNonceBitmap(user.publicKey, new anchor.BN(nonceWordIndex(wrapNonce).toString()))
      .accountsPartial({
        payer: provider.wallet.publicKey,
        nonceBitmap: nonceBitmapPda,
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    await settleForwarderFixture(wrapFixture, wrapSegment(previousEscrowAuthority, previousEscrowAta), [
      wrapAuthorizationIx(user.publicKey, wrapFixture),
    ]);

    assert.equal(await balance(previousEscrowAta), wrapAmount, "the previous build's escrow holds the wrapped tokens");
    assert.equal(
      (await provider.connection.getAccountInfo(nonceBitmapPda))!.data.length,
      PREVIOUS_NONCE_BITMAP_SIZE,
      "the previous build's bitmap is the discriminator and the word",
    );
  });

  it("upgrades the forwarder in place to this build", () => {
    execFileSync(
      "solana",
      [
        "program",
        "deploy",
        "target/deploy/spl_token_forwarder.so",
        "--program-id",
        "target/deploy/spl_token_forwarder-keypair.json",
        "--keypair",
        process.env.ANCHOR_WALLET!,
        "--url",
        provider.connection.rpcEndpoint,
      ],
      { stdio: "inherit" },
    );
  });

  // Only the owner who upgrades the program migrates its accounts, as only
  // the EVM proxy's owner calls upgradeToAndCall.
  it("rejects the migrations from anyone but the upgrade authority", async () => {
    const intruder = await funder.fresh(1);
    const wordIndex = nonceWordIndex(wrapNonce);
    for (const migration of [
      migrateConfig(forwarderProgram, intruder.publicKey),
      migrateNonceBitmap(forwarderProgram, intruder.publicKey, user.publicKey, wordIndex),
      migrateEscrow(forwarderProgram, intruder.publicKey, mint),
    ]) {
      await assertFails(migration.signers([intruder]).rpc(), {
        program: forwarderProgram,
        error: "UnauthorizedCaller",
      });
    }
  });

  // The operator's command, as run on a cluster after the upgrade: it finds
  // every previous-layout bitmap and the owner it was created for.
  const runMigrate = () =>
    execFileSync("npx", ["ts-node", "-P", "tsconfig.json", "scripts/forwarder.ts", "migrate"], {
      env: { ...process.env, STF_TOKEN_MINTS: mint.toBase58() },
      encoding: "utf-8",
    });

  it("migrates the previous build's config, nonce bitmap and escrow through the operator command", async () => {
    const output = runMigrate();
    console.log(output);
    assert.include(output, "1 nonce bitmap(s) were in the previous layout");

    assert.equal(await balance(escrowAta), wrapAmount, "the escrow authority holds the previous escrow's tokens");
    assert.isNull(await provider.connection.getAccountInfo(previousEscrowAta), "the previous escrow account is closed");
  });

  it("finds nothing left to migrate on a second run", () => {
    const output = runMigrate();
    assert.include(output, "is already in this build's layout");
    assert.include(output, "0 nonce bitmap(s) were in the previous layout");
    assert.include(output, "has no previous-build escrow");
  });

  it("rejects migrating an account twice", async () => {
    const owner = provider.wallet.publicKey;
    await assertFails(migrateConfig(forwarderProgram, owner).rpc(), {
      program: forwarderProgram,
      error: "NotPreviousLayout",
    });
    await assertFails(migrateNonceBitmap(forwarderProgram, owner, user.publicKey, nonceWordIndex(wrapNonce)).rpc(), {
      program: forwarderProgram,
      error: "NotPreviousLayout",
    });
  });

  it("keeps the config at the size initialize creates", async () => {
    const config = await provider.connection.getAccountInfo(configPda);
    assert.equal(config!.data.length, 8 + 4 * 32, "discriminator and four 32-byte fields");
    const decoded = await forwarderProgram.account.config.fetch(configPda);
    assert.deepEqual(Array.from(decoded.logicRef), Array.from(Buffer.from(wrap.logic_ref_b64, "base64")));
  });

  it("keeps the previous build's nonce bitmap: the wrap's nonce stays used", async () => {
    const bitmap = await forwarderProgram.account.nonceBitmap.fetch(nonceBitmapPda);
    const bit = Number(wrapNonce % NONCES_PER_WORD);
    assert.ok(bitmap.bits[bit >> 3] & (1 << (bit & 7)), "the wrap's nonce is still marked used");
    assert.equal(bitmap.bump, nonceBitmapBump, "the bitmap records its canonical bump");
  });

  it("rejects a wrap that replays the nonce the previous build recorded", async () => {
    await approve(provider.connection, user, userAta, escrowAuthority, user, Number(wrapAmount));
    await assertFails(
      settleForwarderFixture(wrapReplayFixture, wrapSegment(escrowAuthority, escrowAta), [
        wrapAuthorizationIx(user.publicKey, wrapReplayFixture),
      ]),
      { program: forwarderProgram, error: "NonceAlreadyUsed" },
    );
  });

  it("unwraps the tokens the previous build escrowed", async () => {
    const recipientBefore = await balance(recipientAta);
    await settleForwarderFixture(
      unwrapFixture,
      [...segmentHead, ...escrowTransferAccounts(escrowAta, recipientAta, escrowAuthority)],
      [],
    );
    assert.equal(await balance(recipientAta), recipientBefore + unwrapAmount, "the recipient receives the tokens");
  });
});
