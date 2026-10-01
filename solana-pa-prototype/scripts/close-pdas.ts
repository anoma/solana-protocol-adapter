/**
 * Close every marker PDA (nullifier and root markers) the PA program owns,
 * reclaiming their rent to the signing wallet, the program's upgrade authority.
 *
 * Usage:
 *   ANCHOR_PROVIDER_URL=<rpc> \
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
 * again. Its rent is left unrecovered on purpose.
 */
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { closeAllMarkers } from "../client/devTeardown";
import { derivePaStatePda } from "../client/pda";

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

  const closed = await closeAllMarkers(program, wallet.publicKey);
  console.log(`✅ Closed ${closed} markers; PAState left open by design (closing it would permit a re-init bypass)`);
}

main().catch((err) => {
  console.error("Close PDAs failed:", err);
  process.exit(1);
});
