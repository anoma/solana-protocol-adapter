/**
 * Write a program's production IDL to its canonical Program Metadata IDL
 * account. Run through ops.sh (`idl-publish`), which sets the endpoint and
 * wallet:
 *
 *   npx ts-node -P tsconfig.json scripts/publish-idl.ts <idl json>
 *
 * The wallet must be the program's upgrade authority or the account's
 * explicit authority (client/programMetadata.ts).
 */
import { publishIdl } from "../client/programMetadata";
import { fail, requireEnv } from "./cli-utils";

async function main() {
  const [idlPath] = process.argv.slice(2);
  if (!idlPath) fail("usage: publish-idl.ts <idl json>");
  const writer = await publishIdl(
    requireEnv("ANCHOR_PROVIDER_URL", "the RPC endpoint"),
    requireEnv("ANCHOR_WALLET", "the signing keypair file"),
    idlPath,
  );
  console.log(`IDL ${idlPath} written as the ${writer}.`);
}

main().catch((err) => {
  console.error("❌ publish-idl failed:", err);
  process.exit(1);
});
