import * as anchor from "@anchor-lang/core";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { denyLogicRef } from "../client/instructions";
import { requireHexBytes } from "./cli-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const logicRef = requireHexBytes(
    "PA_DENIED_LOGIC_REF",
    32,
    "the logic ref (hex, 32 bytes) no settlement may consume or create a resource with from now on.\n" +
      "   A denial cannot be undone.",
  );

  await denyLogicRef(program, provider.wallet.publicKey, logicRef).rpc();

  console.log(`✅ Denied logic ref ${Buffer.from(logicRef).toString("hex")}`);
}

main().catch((err) => {
  console.error("❌ Denying the logic ref failed:", err.message || err);
  process.exit(1);
});
