import * as anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import * as fs from "fs";
import * as path from "path";

async function main() {
  const idlPath = path.resolve(
    __dirname,
    "../target/idl/solana_pa_prototype.json"
  );
  const idl = JSON.parse(fs.readFileSync(idlPath, "utf-8"));
  const programId = new PublicKey(idl.address);

  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const program = new anchor.Program(idl, provider);

  const [paStatePda] = PublicKey.findProgramAddressSync(
    [Buffer.from("pa_state")],
    programId
  );
  console.log(`PA program: ${programId.toBase58()}`);
  console.log(`PAState PDA: ${paStatePda.toBase58()}`);

  console.log("Calling emergency_stop...");
  const sig = await program.methods.emergencyStop().accounts({}).rpc();
  console.log(`  ✅ Stopped: ${sig}`);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
