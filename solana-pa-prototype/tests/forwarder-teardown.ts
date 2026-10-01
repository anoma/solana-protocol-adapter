/**
 * SPL token forwarder teardown on a paused adapter: the committee reclaims
 * rent from nonce bitmaps, escrows and finally the config, so this runs
 * among the suite's last files, on the deployment's config. The before hook
 * creates a nonce bitmap (init_nonce_bitmap is permissionless) and an escrow,
 * then stops the adapter unless a file before it did.
 */
import * as anchor from "@anchor-lang/core";
import { Keypair, PublicKey, SystemProgram } from "@solana/web3.js";
import { getAccount, getOrCreateAssociatedTokenAccount } from "@solana/spl-token";
import { assert } from "chai";
import {
  localCloseAllNonceBitmaps,
  localCloseConfig,
  localCloseEscrow,
  localCloseNonceBitmapsBatch,
} from "./utils/localOnly";
import { deriveConfigPda, deriveNonceBitmapPda } from "../client/pda";
import { createFundedEscrow, makeFunder, assertFails } from "./utils/helpers";
import {
  ensureAdapterInitialized,
  ensureForwarderConfig,
  ensurePaused,
  forwarderCommittee as emergencyCommittee,
  forwarderProgram,
  paState,
  provider,
} from "./utils/adapterSuite";

describe("forwarder teardown (reclaims forwarder rent)", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const impostor = Keypair.generate();

  let escrow: { mint: PublicKey; escrowAta: PublicKey; recipientAta: PublicKey };
  const ESCROW_FUNDING = 500_000_000n;

  before(async () => {
    await funder.fund(emergencyCommittee, 5);
    await funder.fund(impostor, 1);

    await ensureAdapterInitialized();
    await ensureForwarderConfig();
    const bitmapUser = Keypair.generate().publicKey;
    await forwarderProgram.methods
      .initNonceBitmap(bitmapUser, new anchor.BN(0))
      .accountsPartial({
        payer: provider.wallet.publicKey,
        nonceBitmap: deriveNonceBitmapPda(forwarderProgram.programId, bitmapUser, 0n)[0],
        systemProgram: SystemProgram.programId,
      })
      .rpc();

    const funded = await createFundedEscrow(provider, forwarderProgram.programId, emergencyCommittee, ESCROW_FUNDING);
    const recipientAta = (
      await getOrCreateAssociatedTokenAccount(
        provider.connection,
        emergencyCommittee,
        funded.mint,
        emergencyCommittee.publicKey,
      )
    ).address;
    escrow = { ...funded, recipientAta };

    await ensurePaused();
  });

  const closeEscrowAs = (authority: Keypair, accounts = escrow) =>
    localCloseEscrow(forwarderProgram, authority.publicKey, paState, accounts).signers([authority]).rpc();

  const closeConfigAs = (authority: Keypair) =>
    localCloseConfig(forwarderProgram, authority.publicKey, paState).signers([authority]).rpc();

  it("close_escrow rejects a non-committee authority", () =>
    assertFails(closeEscrowAs(impostor), { program: forwarderProgram, error: "UnauthorizedCaller" }));

  it("close_nonce_bitmaps_batch rejects a non-committee authority", () =>
    assertFails(localCloseAllNonceBitmaps(forwarderProgram, impostor.publicKey, paState, [impostor]), {
      program: forwarderProgram,
      error: "UnauthorizedCaller",
    }));

  it("close_config rejects a non-committee authority", () =>
    assertFails(closeConfigAs(impostor), { program: forwarderProgram, error: "UnauthorizedCaller" }));

  it("close_nonce_bitmaps_batch rejects a program account that is not a bitmap", () =>
    // The config PDA is program-owned but is not a nonce bitmap.
    assertFails(
      localCloseNonceBitmapsBatch(forwarderProgram, emergencyCommittee.publicKey, paState, [configPda])
        .signers([emergencyCommittee])
        .rpc(),
      { program: forwarderProgram, error: "InvalidNonceBitmapPda" },
    ));

  it("closes every nonce bitmap and refunds their rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    const closed = await localCloseAllNonceBitmaps(forwarderProgram, emergencyCommittee.publicKey, paState, [
      emergencyCommittee,
    ]);

    assert.isAbove(closed, 0, "the before hook created a nonce bitmap to close");
    assert.isEmpty(await forwarderProgram.account.nonceBitmap.all(), "every nonce bitmap is closed");
    assert.isAbove(
      await provider.connection.getBalance(emergencyCommittee.publicKey),
      committeeBefore,
      "the committee recovers more rent than it pays in fees",
    );
  });

  it("close_escrow drains the tokens to the recipient and closes the account", async () => {
    assert.equal((await getAccount(provider.connection, escrow.escrowAta)).amount, ESCROW_FUNDING);
    const recipientBefore = (await getAccount(provider.connection, escrow.recipientAta)).amount;
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await closeEscrowAs(emergencyCommittee);

    assert.isNull(await provider.connection.getAccountInfo(escrow.escrowAta), "escrow ATA is closed");
    const recipientAfter = (await getAccount(provider.connection, escrow.recipientAta)).amount;
    assert.equal(recipientAfter - recipientBefore, ESCROW_FUNDING, "every token is drained to the recipient");
    assert.isAbove(
      await provider.connection.getBalance(emergencyCommittee.publicKey),
      committeeBefore,
      "rent returns to the committee",
    );
  });

  it("close_escrow closes an empty escrow", async () => {
    const { mint, ...empty } = await createFundedEscrow(provider, forwarderProgram.programId, emergencyCommittee, 0n);
    const recipientAta = (
      await getOrCreateAssociatedTokenAccount(
        provider.connection,
        emergencyCommittee,
        mint,
        emergencyCommittee.publicKey,
      )
    ).address;
    assert.equal((await getAccount(provider.connection, empty.escrowAta)).amount, 0n);

    await closeEscrowAs(emergencyCommittee, { mint, ...empty, recipientAta });

    assert.isNull(await provider.connection.getAccountInfo(empty.escrowAta), "empty escrow ATA is closed");
  });

  it("close_config closes the config last and refunds its rent", async () => {
    const committeeBefore = await provider.connection.getBalance(emergencyCommittee.publicKey);

    await closeConfigAs(emergencyCommittee);

    assert.isNull(await provider.connection.getAccountInfo(configPda), "config PDA is closed");
    assert.isAbove(
      await provider.connection.getBalance(emergencyCommittee.publicKey),
      committeeBefore,
      "rent returns to the committee",
    );
  });

  after(() => funder.drainAll());
});
