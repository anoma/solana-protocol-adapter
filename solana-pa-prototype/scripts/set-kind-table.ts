import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { setKindTableRoot } from "../client/instructions";
import { requireHexBytes } from "./cli-utils";

async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const kindTableRoot = requireHexBytes(
    "PA_KIND_TABLE_ROOT",
    32,
    "the Merkle root (hex, 32 bytes) of the kind table every settled aggregation instance must carry from now on.\n" +
      "   Transactions proven against the previous table are rejected once this is installed.",
  );

  await setKindTableRoot(program, provider.wallet.publicKey, kindTableRoot).rpc();

  console.log(`✅ Kind table root: ${Buffer.from(kindTableRoot).toString("hex")}`);
}

main().catch((err) => {
  console.error("❌ Setting the kind table root failed:", err.message || err);
  process.exit(1);
});
