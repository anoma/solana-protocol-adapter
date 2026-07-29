/**
 * Close all PDA accounts owned by the PA program, reclaiming rent to the authority.
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=https://api.devnet.solana.com \
 *   ANCHOR_WALLET=scripts/devnet-wallet.json \
 *   npx ts-node -P tsconfig.json scripts/close-pdas.ts
 *
 * What it does:
 *   1. Finds all 0-byte marker accounts via getProgramAccounts
 *   2. Closes markers in batches (via close_markers_batch)
 *
 * The PAState account is deliberately NOT closed and there is no instruction to
 * close it. Closing it would allow re-initialization, and because the PA state
 * PDA derives from a fixed seed, a re-initialized adapter reuses the same marker
 * addresses — so previously closed nullifier markers would become spendable
 * again. `close_pa_state` was removed for exactly this reason (commit deefa91,
 * "remove close_pa_state ... preventing the reinit bypass"). Its rent is left
 * unrecovered on purpose.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import { PA_STATE_SEED } from "../tests/utils/constants";

/**
 * Fail loudly with an actionable message when the deployed program's IDL
 * doesn't define `instructionName`, instead of letting `program.methods.*`
 * throw an opaque "is not a function" TypeError deep inside a batch loop.
 *
 * An instruction is absent when the program was built without the Cargo feature
 * that defines it — `close_markers_batch` requires `dev-teardown`, which
 * production builds do not enable.
 */
function requireInstruction(
  program: Program<SolanaPaPrototype>,
  instructionName: string
): void {
  const present = program.idl.instructions.some(
    (ix) => ix.name === instructionName
  );
  if (!present) {
    console.error(
      `❌ Instruction '${instructionName}' is not present in the deployed ` +
      `program's IDL (target/idl/solana_pa_prototype.json).\n` +
      `   Either the program was built without the Cargo feature that ` +
      `defines it (e.g. 'dev-teardown' for close_markers_batch), or the ` +
      `instruction does not exist on this branch. This script cannot proceed.`
    );
    process.exit(1);
  }
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;
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

  const paState = await program.account.paStateAccount.fetch(paStatePda);
  if (!paState.authority.equals(wallet.publicKey)) {
    console.error(
      `❌ Wallet ${wallet.publicKey.toBase58()} is not the PA authority ` +
      `(${paState.authority.toBase58()})`
    );
    process.exit(1);
  }

  let totalRecovered = 0;

  {
    requireInstruction(program, "close_markers_batch");

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
            `${batch.length} markers (~${(batchLamports / LAMPORTS_PER_SOL).toFixed(6)} SOL)`
          );
        } catch (e: any) {
          console.error(`  ❌ Batch ${Math.floor(i / BATCH_SIZE) + 1} failed: ${e.message}`);
        }
      }
    }
  }

  console.log(
    "\nPAState left open by design — closing it would permit a re-init bypass."
  );

  console.log(`\n✅ Total recovered: ${(totalRecovered / LAMPORTS_PER_SOL).toFixed(6)} SOL`);
}

main().catch((err) => {
  console.error("Close PDAs failed:", err);
  process.exit(1);
});
