/**
 * Close all PDA accounts owned by the PA program, reclaiming rent to the authority.
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=scripts/devnet-wallet.json \
 *   npx ts-node -P tsconfig.json scripts/close-pdas.ts [--pa-state-only]
 *
 * What it does:
 *   1. Finds all 0-byte marker accounts via getProgramAccounts
 *   2. Closes markers in batches (via close_markers_batch)
 *   3. Closes PAState (via close_pa_state) — last, since markers reference it
 *
 * Flags:
 *   --pa-state-only   Only close the PAState account (for re-initialization after upgrade)
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, LAMPORTS_PER_SOL } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import { PA_STATE_SEED } from "../tests/utils/constants";

async function main() {
  const paStateOnly = process.argv.includes("--pa-state-only");

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace
    .SolanaPaPrototype as Program<SolanaPaPrototype>;
  const connection = provider.connection;
  const wallet = provider.wallet as anchor.Wallet;

  console.log("Program:  ", program.programId.toBase58());
  console.log("Authority:", wallet.publicKey.toBase58());

  const [paStatePda] = PublicKey.findProgramAddressSync(
    [PA_STATE_SEED],
    program.programId
  );

  const paStateInfo = await connection.getAccountInfo(paStatePda);
  if (!paStateInfo) {
    console.log("PAState not found — nothing to close.");
    return;
  }

  // Read authority directly from raw data to handle layout migrations.
  // Layout: discriminator(8) + bump(1) + authority(32)
  const authorityBytes = paStateInfo.data.subarray(9, 41);
  const authority = new PublicKey(authorityBytes);
  if (!authority.equals(wallet.publicKey)) {
    console.error(
      `❌ Wallet ${wallet.publicKey.toBase58()} is not the PA authority ` +
        `(${authority.toBase58()})`
    );
    process.exit(1);
  }

  let totalRecovered = 0;

  if (!paStateOnly) {
    // Find all 0-byte marker accounts (nullifier + root markers)
    const markers = await connection.getProgramAccounts(program.programId, {
      filters: [{ dataSize: 0 }],
    });
    console.log(`\nFound ${markers.length} marker accounts`);

    if (markers.length > 0) {
      const BATCH_SIZE = 20;
      const markerRent = await connection.getMinimumBalanceForRentExemption(0);
      console.log(`Closing in batches of ${BATCH_SIZE}...`);

      const markerPubkeys = markers.map(({ pubkey }) => pubkey);
      for (let i = 0; i < markerPubkeys.length; i += BATCH_SIZE) {
        const batch = markerPubkeys.slice(i, i + BATCH_SIZE);
        const remainingAccounts = batch.map((pubkey) => ({
          pubkey,
          isWritable: true,
          isSigner: false,
        }));

        try {
          await program.methods
            .closeMarkersBatch()
            .accounts({
              paState: paStatePda,
              authority: wallet.publicKey,
            })
            .remainingAccounts(remainingAccounts)
            .rpc();

          const batchLamports = batch.length * markerRent;
          totalRecovered += batchLamports;
          console.log(
            `  Batch ${Math.floor(i / BATCH_SIZE) + 1}: ` +
              `${batch.length} markers (~${(
                batchLamports / LAMPORTS_PER_SOL
              ).toFixed(6)} SOL)`
          );
        } catch (e: any) {
          console.error(
            `  ❌ Batch ${Math.floor(i / BATCH_SIZE) + 1} failed: ${e.message}`
          );
        }
      }
    }
  }

  // Close PAState last
  console.log("\nClosing PAState...");
  const paStateLamports = paStateInfo.lamports;
  try {
    await program.methods.closePaState().accounts({}).rpc();
    totalRecovered += paStateLamports;
    console.log(
      `  ✅ PAState closed (${(paStateLamports / LAMPORTS_PER_SOL).toFixed(
        6
      )} SOL)`
    );
  } catch (e: any) {
    console.error(`  ❌ Failed to close PAState: ${e.message}`);
  }

  console.log(
    `\n✅ Total recovered: ${(totalRecovered / LAMPORTS_PER_SOL).toFixed(
      6
    )} SOL`
  );
}

main().catch((err) => {
  console.error("Close PDAs failed:", err);
  process.exit(1);
});
