/**
 * PA Close Instruction Tests
 *
 * Extracted from solana-pa-prototype.ts so they run AFTER zz-forwarder-emergency.
 * The emergency tests need PAState alive; these tests destroy it.
 *
 * Order: zz-forwarder-emergency (stops PA) → zzz-pa-teardown (closes PA PDAs)
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  Keypair,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import { assert } from "chai";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import {
  PA_STATE_SEED,
  AUTHORITY_MISMATCH_PATTERN,
} from "./utils";
import { fundKeypair } from "./utils";

describe("zzz-pa-teardown (close PA PDAs — runs after emergency tests)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace
    .SolanaPaPrototype as Program<SolanaPaPrototype>;
  const [paState] = PublicKey.findProgramAddressSync(
    [PA_STATE_SEED],
    program.programId
  );

  it("close_markers_batch closes marker PDAs and refunds rent", async () => {
    const allAccounts = await provider.connection.getProgramAccounts(
      program.programId,
      { filters: [{ dataSize: 0 }] }
    );

    if (allAccounts.length === 0) {
      console.log("    No markers to close (no settlements ran)");
      return;
    }

    const markersBefore = allAccounts.length;
    const balanceBefore = await provider.connection.getBalance(
      provider.wallet.publicKey
    );

    const remainingAccounts = allAccounts.map(({ pubkey }) => ({
      pubkey,
      isWritable: true,
      isSigner: false,
    }));

    await program.methods
      .closeMarkersBatch()
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .remainingAccounts(remainingAccounts)
      .rpc();

    const markersAfter = await provider.connection.getProgramAccounts(
      program.programId,
      { filters: [{ dataSize: 0 }] }
    );
    assert.equal(markersAfter.length, 0, "All markers should be closed");

    const balanceAfter = await provider.connection.getBalance(
      provider.wallet.publicKey
    );
    assert.ok(
      balanceAfter > balanceBefore,
      "Authority should have received rent refund"
    );
    console.log(
      `    Closed ${markersBefore} markers, recovered ${((balanceAfter - balanceBefore) / LAMPORTS_PER_SOL).toFixed(6)} SOL`
    );
  });

  it("close_markers_batch rejects non-authority", async () => {
    const fakeAuthority = Keypair.generate();
    await fundKeypair(provider, fakeAuthority, 1);

    try {
      await program.methods
        .closeMarkersBatch()
        .accounts({
          paState,
          authority: fakeAuthority.publicKey,
        })
        .signers([fakeAuthority])
        .rpc();
      assert.fail("Expected unauthorized close to fail");
    } catch (e: any) {
      assert.match(e.toString(), AUTHORITY_MISMATCH_PATTERN);
    }
  });

  it("close_pa_state rejects non-authority", async () => {
    const fakeAuthority = Keypair.generate();
    await fundKeypair(provider, fakeAuthority, 1);

    try {
      await program.methods
        .closePaState()
        .accounts({
          paState,
          authority: fakeAuthority.publicKey,
        })
        .signers([fakeAuthority])
        .rpc();
      assert.fail("Expected unauthorized close to fail");
    } catch (e: any) {
      assert.match(e.toString(), AUTHORITY_MISMATCH_PATTERN);
    }
  });

  it("close_pa_state closes PAState and refunds rent", async () => {
    const paStateInfo = await provider.connection.getAccountInfo(paState);
    assert.ok(paStateInfo, "PAState should exist before close");

    const balanceBefore = await provider.connection.getBalance(
      provider.wallet.publicKey
    );

    await program.methods
      .closePaState()
      .accounts({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const paStateAfter = await provider.connection.getAccountInfo(paState);
    assert.equal(paStateAfter, null, "PAState should be closed");

    const balanceAfter = await provider.connection.getBalance(
      provider.wallet.publicKey
    );
    assert.ok(
      balanceAfter > balanceBefore,
      "Authority should have received rent refund"
    );
    console.log(
      `    PAState closed, recovered ${((balanceAfter - balanceBefore) / LAMPORTS_PER_SOL).toFixed(6)} SOL`
    );
  });
});
