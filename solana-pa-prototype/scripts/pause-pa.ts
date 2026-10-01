/**
 * Pause or unpause settlement, as pa-evm's owner does with `pause()` and
 * `unpause()`. Run through ops.sh (`pause` / `unpause`), which sets the
 * cluster and wallet; the wallet must be the program's upgrade authority.
 *
 *   npx ts-node -P tsconfig.json scripts/pause-pa.ts <pause|unpause>
 *
 * Idempotent: an adapter already in the requested state is left as it is.
 */
import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { pauseAdapter, unpauseAdapter } from "../client/instructions";
import { derivePaStatePda } from "../client/pda";

async function main() {
  const action = process.argv[2];
  if (action !== "pause" && action !== "unpause") {
    throw new Error(`usage: pause-pa.ts <pause|unpause>, got ${action}`);
  }
  const pause = action === "pause";

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const [paState] = derivePaStatePda(program.programId);

  const account = await program.account.paStateAccount.fetchNullable(paState);
  if (!account) {
    throw new Error(`PAState (${paState.toBase58()}) does not exist: the PA is not initialized`);
  }
  if (account.paused === pause) {
    console.log(`PA is already ${pause ? "paused" : "running"}.`);
    return;
  }

  console.log(`${pause ? "Pausing" : "Unpausing"} PA ${program.programId.toBase58()} (PAState ${paState.toBase58()})`);
  const signature = await (pause ? pauseAdapter : unpauseAdapter)(program, provider.wallet.publicKey).rpc();

  // Verify the on-chain result rather than assuming the transaction did
  // what was expected.
  const after = await program.account.paStateAccount.fetch(paState);
  if (after.paused !== pause) {
    throw new Error(`transaction ${signature} landed but PAState.paused is still ${after.paused}`);
  }
  console.log(`✅ PA ${pause ? "paused" : "running"}. Signature: ${signature}`);
}

main().catch((err) => {
  console.error(`❌ ${process.argv[2]} failed:`, err);
  process.exit(1);
});
