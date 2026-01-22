/**
 * Simple devnet settlement test.
 * Tests that the PA can call groth16_verifier directly on devnet.
 *
 * Run with: yarn run ts-mocha -p ./tsconfig.json -t 1000000 tests/devnet-settle-test.ts
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
  ComputeBudgetProgram,
  SYSVAR_CLOCK_PUBKEY,
} from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import path from "path";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

// Groth16 verifier program ID (deployed on devnet by risc0)
const GROTH16_VERIFIER_ID = new PublicKey("THq1qFYQoh7zgcjXoMXduDBqiZRCPeg3PvvMbrVQUge");

type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string;
  tx_b64: string;
  tx_tampered_b64: string;
  consumed_nullifiers_b64: string[];
  created_commitments_b64?: string[];
};

function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

async function airdrop(provider: anchor.AnchorProvider, to: PublicKey, sol: number) {
  const sig = await provider.connection.requestAirdrop(to, sol * LAMPORTS_PER_SOL);
  await provider.connection.confirmTransaction(sig, "confirmed");
}

describe("Devnet Settlement Test", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const fixturePath = path.resolve(process.cwd(), "tests", "fixtures", "batch_groth16.json");
  const fixture = readJson<Fixture>(fixturePath);
  const tx = Buffer.from(fixture.tx_b64, "base64");

  const PA_STATE_SEED = Buffer.from("pa_state");
  const NULLIFIER_SEED = Buffer.from("nullifier");
  const TX_DATA_SEED = Buffer.from("tx_data");

  const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);
  const nullifierPdas = fixture.consumed_nullifiers_b64.map((nfB64) => {
    const nf = Buffer.from(nfB64, "base64");
    return PublicKey.findProgramAddressSync([NULLIFIER_SEED, paState.toBuffer(), nf], program.programId)[0];
  });

  const remainingAccounts = nullifierPdas.map((pubkey) => ({
    pubkey,
    isWritable: true,
    isSigner: false,
  }));

  // Block-time-forwarder on devnet
  const blockTimeForwarderId = new PublicKey("FFLBDCaVJkdamigjmRjzhWPR2rJ4N6k4QpiBiR4Npq7d");

  it("initializes PA state if needed", async () => {
    try {
      const state = await program.account.paStateAccount.fetch(paState);
      console.log("PA State already exists:", paState.toBase58());
      console.log("  Authority:", state.authority.toBase58());
      console.log("  Next Index:", state.nextIndex.toString());
      console.log("  Current Depth:", state.currentDepth);
      console.log("  Paused:", state.paused);
    } catch {
      console.log("PA State not found, initializing...");
      const authority = (provider.wallet as any).payer as Keypair;

      // Compute genesis root marker PDA (for remaining_accounts)
      // PADDING_LEAF = ZEROS[0] from merkle.rs - this is the initial tree root
      const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
        "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
        "hex"
      );
      const ROOT_MARKER_SEED = Buffer.from("root");
      const [genesisRootMarkerPda] = PublicKey.findProgramAddressSync(
        [ROOT_MARKER_SEED, paState.toBuffer(), EMPTY_TREE_ROOT_INITIAL],
        program.programId
      );

      await program.methods
        .initialize()
        .accounts({
          paState,
          payer: authority.publicKey,
          systemProgram: SystemProgram.programId,
        } as any)
        .remainingAccounts([
          { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
        ])
        .signers([authority])
        .rpc();

      const state = await program.account.paStateAccount.fetch(paState);
      console.log("PA State initialized:", paState.toBase58());
      console.log("  Authority:", state.authority.toBase58());
    }
    assert.ok(true, "PA state should exist");
  });

  it("verifies groth16_verifier program exists on devnet", async () => {
    const info = await provider.connection.getAccountInfo(GROTH16_VERIFIER_ID);
    assert.ok(info, "groth16_verifier should exist on devnet");
    assert.ok(info!.executable, "groth16_verifier should be executable");
    console.log("groth16_verifier exists:", GROTH16_VERIFIER_ID.toBase58());
  });

  it("settles a transaction via groth16_verifier (devnet)", async () => {
    // Use provider wallet directly (avoid airdrop rate limits on devnet)
    // The provider wallet must have funds from a prior transfer
    const balance = await provider.connection.getBalance(provider.wallet.publicKey);
    console.log("Provider wallet balance:", balance / LAMPORTS_PER_SOL, "SOL");
    if (balance < 0.5 * LAMPORTS_PER_SOL) {
      console.log("Skipping test - insufficient funds in provider wallet");
      return;
    }

    // Use provider wallet as authority (it's already funded)
    const authority = (provider.wallet as any).payer as Keypair;

    const uploadId = new anchor.BN(Date.now());
    const uploadIdLe = Buffer.alloc(8);
    uploadIdLe.writeBigUInt64LE(BigInt(uploadId.toString()));

    const [txData] = PublicKey.findProgramAddressSync(
      [TX_DATA_SEED, authority.publicKey.toBuffer(), uploadIdLe],
      program.programId
    );

    const capacity = tx.length;
    const slot = await provider.connection.getSlot("confirmed");
    const expiresSlot = new anchor.BN(slot + 10_000);

    console.log("Initializing TxData account...");
    await program.methods
      .txdataInit(uploadId, capacity, expiresSlot)
      .accounts({
        paState,
        txData,
        authority: authority.publicKey,
        systemProgram: SystemProgram.programId,
      } as any)
      .signers([authority])
      .rpc();

    console.log("Uploading tx data in chunks...");
    const chunkSize = 700;
    for (let offset = 0; offset < tx.length; offset += chunkSize) {
      const chunk = tx.subarray(offset, Math.min(tx.length, offset + chunkSize));
      await program.methods
        .txdataWrite(uploadId, offset, Buffer.from(chunk))
        .accounts({
          txData,
          authority: authority.publicKey,
        } as any)
        .signers([authority])
        .rpc();
      console.log(`  Uploaded chunk at offset ${offset}`);
    }

    const allRemainingAccounts = [
      ...remainingAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: SYSVAR_CLOCK_PUBKEY, isWritable: false, isSigner: false },
    ];

    console.log("Calling settleFromTxdata with groth16_verifier...");
    try {
      const sig = await program.methods
        .settleFromTxdata(uploadId)
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          groth16VerifierProgram: GROTH16_VERIFIER_ID,
        } as any)
        .remainingAccounts(allRemainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([authority])
        .rpc();

      console.log("Settlement successful! Signature:", sig);

      // Verify state was updated
      const stateAfter = await program.account.paStateAccount.fetch(paState);
      console.log("State after settlement:");
      console.log("  Next Index:", stateAfter.nextIndex.toString());

      assert.ok(true, "Settlement succeeded");
    } catch (e: any) {
      console.error("Settlement failed:");
      console.error("  Error:", e?.error?.errorMessage ?? e?.message ?? e);
      if (e?.logs) {
        console.error("  Logs:", e.logs.join("\n"));
      }
      throw e;
    }
  });
});
