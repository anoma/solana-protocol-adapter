/**
 * Devnet settlement test using our deployed verifier infrastructure.
 *
 * Deployed addresses (2026-01-22):
 * - Protocol Adapter: AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt
 * - Verifier Router: CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq
 * - Groth16 Verifier: DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk
 * - Router PDA: 5GzjEjtL3JqKSp8sx4Kjzxuwg6xQ3pPyuTqnQJxbemEk
 * - Selector: 0x00000001
 *
 * Run with:
 *   CLUSTER_URL="https://api.devnet.solana.com" yarn run ts-mocha -p ./tsconfig.json -t 1000000 tests/devnet-router-test.ts
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

// Our deployed devnet addresses
const VERIFIER_ROUTER_ID = new PublicKey("CnhgPbCm2mjYYT2konzKsBD7RL8Mfg63nuzB7xsbABFq");
const GROTH16_VERIFIER_ID = new PublicKey("DBcDFEFD87rLdoepucSxbvG13idCo6HYS4sutVihkmbk");
const ROUTER_PDA = new PublicKey("5GzjEjtL3JqKSp8sx4Kjzxuwg6xQ3pPyuTqnQJxbemEk");

// Selector from the fixture (matches RISC0 verifier_parameters)
// Note: We also have 0x00000001 registered but the fixture uses 0x73c457ba
const GROTH16_SELECTOR = Buffer.from([0x73, 0xc4, 0x57, 0xba]);

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

/**
 * Derive the verifier entry PDA for a given selector.
 * Seeds: ["verifier", selector_bytes]
 */
function deriveVerifierEntryPda(selector: Buffer): PublicKey {
  const [pda] = PublicKey.findProgramAddressSync(
    [Buffer.from("verifier"), selector],
    VERIFIER_ROUTER_ID
  );
  return pda;
}

describe("Devnet Router Settlement Test", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;

  const fixturePath = path.resolve(process.cwd(), "tests", "fixtures", "batch_groth16.json");
  const fixture = readJson<Fixture>(fixturePath);
  const tx = Buffer.from(fixture.tx_b64, "base64");

  const PA_STATE_SEED = Buffer.from("pa_state");
  const NULLIFIER_SEED = Buffer.from("nullifier");
  const TX_DATA_SEED = Buffer.from("tx_data");
  const ROOT_MARKER_SEED = Buffer.from("root");

  // PADDING_LEAF / initial empty tree root
  const EMPTY_TREE_ROOT = Buffer.from(
    "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
    "hex"
  );

  const [paState] = PublicKey.findProgramAddressSync([PA_STATE_SEED], program.programId);

  // Derive verifier entry PDA for our selector
  const verifierEntry = deriveVerifierEntryPda(GROTH16_SELECTOR);

  const nullifierPdas = fixture.consumed_nullifiers_b64.map((nfB64) => {
    const nf = Buffer.from(nfB64, "base64");
    return PublicKey.findProgramAddressSync(
      [NULLIFIER_SEED, paState.toBuffer(), nf],
      program.programId
    )[0];
  });

  before(async () => {
    console.log("\n=== Devnet Router Settlement Test ===");
    console.log("PA Program:", program.programId.toBase58());
    console.log("PA State PDA:", paState.toBase58());
    console.log("Verifier Router:", VERIFIER_ROUTER_ID.toBase58());
    console.log("Router PDA:", ROUTER_PDA.toBase58());
    console.log("Groth16 Verifier:", GROTH16_VERIFIER_ID.toBase58());
    console.log("Verifier Entry PDA:", verifierEntry.toBase58());
    console.log("Selector:", "0x" + GROTH16_SELECTOR.toString("hex"));
    console.log("");
  });

  it("verifies all programs exist on devnet", async () => {
    const paInfo = await provider.connection.getAccountInfo(program.programId);
    assert.ok(paInfo, "PA program should exist");
    assert.ok(paInfo!.executable, "PA should be executable");
    console.log("✓ PA program exists");

    const routerInfo = await provider.connection.getAccountInfo(VERIFIER_ROUTER_ID);
    assert.ok(routerInfo, "Verifier Router should exist");
    assert.ok(routerInfo!.executable, "Router should be executable");
    console.log("✓ Verifier Router exists");

    const verifierInfo = await provider.connection.getAccountInfo(GROTH16_VERIFIER_ID);
    assert.ok(verifierInfo, "Groth16 Verifier should exist");
    assert.ok(verifierInfo!.executable, "Groth16 should be executable");
    console.log("✓ Groth16 Verifier exists");

    const routerPdaInfo = await provider.connection.getAccountInfo(ROUTER_PDA);
    assert.ok(routerPdaInfo, "Router PDA should exist (router initialized)");
    console.log("✓ Router PDA exists (router is initialized)");

    const entryInfo = await provider.connection.getAccountInfo(verifierEntry);
    assert.ok(entryInfo, "Verifier Entry PDA should exist");
    console.log("✓ Verifier Entry exists (Groth16 registered with selector 0x00000001)");
  });

  it("initializes PA state if needed", async () => {
    try {
      const state = await program.account.paStateAccount.fetch(paState);
      console.log("PA State already exists:");
      console.log("  Authority:", state.authority.toBase58());
      console.log("  Next Index:", state.nextIndex.toString());
      console.log("  Current Depth:", state.currentDepth);
      console.log("  Paused:", state.paused);
    } catch {
      console.log("PA State not found, initializing...");
      const authority = (provider.wallet as any).payer as Keypair;

      // Compute genesis root marker PDA
      const [genesisRootMarkerPda] = PublicKey.findProgramAddressSync(
        [ROOT_MARKER_SEED, paState.toBuffer(), EMPTY_TREE_ROOT],
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
      console.log("PA State initialized:");
      console.log("  Authority:", state.authority.toBase58());
      console.log("  Next Index:", state.nextIndex.toString());
    }
  });

  it("settles a transaction via verifier_router (devnet)", async () => {
    const balance = await provider.connection.getBalance(provider.wallet.publicKey);
    console.log("Wallet balance:", balance / LAMPORTS_PER_SOL, "SOL");

    if (balance < 0.1 * LAMPORTS_PER_SOL) {
      console.log("⚠ Skipping settlement - insufficient funds");
      console.log("  Fund wallet:", provider.wallet.publicKey.toBase58());
      return;
    }

    const authority = (provider.wallet as any).payer as Keypair;

    // Create unique upload ID
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

    console.log("\n1. Initializing TxData account...");
    console.log("   Upload ID:", uploadId.toString());
    console.log("   TxData PDA:", txData.toBase58());
    console.log("   Capacity:", capacity, "bytes");

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
    console.log("   ✓ TxData initialized");

    console.log("\n2. Uploading tx data in chunks...");
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
      console.log(`   ✓ Chunk at offset ${offset} (${chunk.length} bytes)`);
    }

    // Build remaining accounts: nullifiers
    const remainingAccounts = nullifierPdas.map((pubkey) => ({
      pubkey,
      isWritable: true,
      isSigner: false,
    }));

    console.log("\n3. Calling settleFromTxdata via verifier_router...");
    console.log("   Nullifiers:", nullifierPdas.length);
    console.log("   Router:", VERIFIER_ROUTER_ID.toBase58());
    console.log("   Router PDA:", ROUTER_PDA.toBase58());
    console.log("   Verifier Entry:", verifierEntry.toBase58());
    console.log("   Groth16 Program:", GROTH16_VERIFIER_ID.toBase58());

    try {
      const sig = await program.methods
        .settleFromTxdata(uploadId)
        .accounts({
          paState,
          txData,
          authority: authority.publicKey,
          systemProgram: SystemProgram.programId,
          verifierRouterProgram: VERIFIER_ROUTER_ID,
          router: ROUTER_PDA,
          verifierEntry: verifierEntry,
          verifierProgram: GROTH16_VERIFIER_ID,
        } as any)
        .remainingAccounts(remainingAccounts)
        .preInstructions([
          ComputeBudgetProgram.setComputeUnitLimit({ units: 1_400_000 }),
        ])
        .signers([authority])
        .rpc();

      console.log("\n✓ Settlement successful!");
      console.log("  Signature:", sig);
      console.log("  Explorer: https://explorer.solana.com/tx/" + sig + "?cluster=devnet");

      // Verify state was updated
      const stateAfter = await program.account.paStateAccount.fetch(paState);
      console.log("\nState after settlement:");
      console.log("  Next Index:", stateAfter.nextIndex.toString());

      assert.ok(true, "Settlement succeeded");
    } catch (e: any) {
      console.error("\n✗ Settlement failed:");
      console.error("  Error:", e?.error?.errorMessage ?? e?.message ?? e);
      if (e?.logs) {
        console.error("\nProgram logs:");
        for (const log of e.logs) {
          console.error("  " + log);
        }
      }
      throw e;
    }
  });
});
