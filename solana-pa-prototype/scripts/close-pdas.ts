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
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { closeAllMarkers } from "../tests/utils/devTeardown";
import { derivePaStatePda } from "../tests/utils/pda";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const connection = provider.connection;
  const wallet = provider.wallet as anchor.Wallet;

  console.log("Program:  ", program.programId.toBase58());
  console.log("Authority:", wallet.publicKey.toBase58());

  const [paStatePda] = derivePaStatePda(program.programId);

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

  const closed = await closeAllMarkers(program, wallet.publicKey);
  console.log(`✅ Closed ${closed} markers; PAState left open by design (closing it would permit a re-init bypass)`);
}

main().catch((err) => {
  console.error("Close PDAs failed:", err);
  process.exit(1);
});
