import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { emergencyStop } from "../tests/utils/helpers";
import { derivePaStatePda } from "../tests/utils/pda";

function isStopped(lifecycle: object): boolean {
  return "stopped" in lifecycle;
}

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const [paState] = derivePaStatePda(program.programId);

  let account;
  try {
    account = await program.account.paStateAccount.fetch(paState);
  } catch {
    console.error(
      `❌ PAState (${paState.toBase58()}) does not exist — the PA is not initialized, so there is nothing to stop.`
    );
    process.exit(1);
  }

  if (!provider.wallet.publicKey.equals(account.authority)) {
    console.error(
      `❌ Wallet ${provider.wallet.publicKey.toBase58()} is not the PA authority (${account.authority.toBase58()}).`
    );
    process.exit(1);
  }

  // Idempotent: a stopped PA is the requested end state.
  if (isStopped(account.lifecycle)) {
    console.log("PA is already stopped.");
    return;
  }

  console.log("Emergency-stopping PA...");
  console.log(`  Program: ${program.programId.toBase58()}`);
  console.log(`  PAState PDA: ${paState.toBase58()}`);

  const signature = await emergencyStop(program, provider.wallet.publicKey).rpc();

  // Verify the on-chain result rather than assuming the transaction did
  // what we expect.
  const after = await program.account.paStateAccount.fetch(paState);
  if (!isStopped(after.lifecycle)) {
    console.error(
      `❌ Transaction ${signature} landed but PAState lifecycle is still not Stopped — investigate before retrying.`
    );
    process.exit(1);
  }

  console.log(`✅ PA stopped. Signature: ${signature}`);
}

main().catch((err) => {
  console.error("❌ Emergency stop failed:", err.message || err);
  process.exit(1);
});
