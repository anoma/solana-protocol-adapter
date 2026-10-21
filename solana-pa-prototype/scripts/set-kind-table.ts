import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { setKindTreeRoot } from "../client/instructions";
import { requireHexBytes } from "./cli-utils";

async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);

  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;

  const kindTreeRoot = requireHexBytes(
    "PA_KIND_TREE_ROOT",
    32,
    "the Merkle root (hex, 32 bytes) of the kind table every settled aggregation instance must carry from now on.\n" +
      "   Transactions proven against the previous table are rejected once this is installed.",
  );

  await setKindTreeRoot(program, provider.wallet.publicKey, kindTreeRoot).rpc();

  console.log(`✅ Kind tree root: ${Buffer.from(kindTreeRoot).toString("hex")}`);
}

main().catch((err) => {
  console.error("❌ Setting the kind tree root failed:", err.message || err);
  process.exit(1);
});
