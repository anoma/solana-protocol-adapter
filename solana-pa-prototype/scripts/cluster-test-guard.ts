/**
 * Refuse a cluster test run whose wallet owns a deployment under test.
 *
 * The cluster test run (`ops.sh test --cluster <c>`) signs with its wallet
 * against a live deployment. Each program's owner is its upgrade authority
 * (the loader's ProgramData records it), so a spec run by that wallet could
 * pause the adapter, replace its kind table, or renounce the authority and
 * leave the program final for good. Run through ops.sh before any spec:
 *
 *   npx ts-node -P tsconfig.json scripts/cluster-test-guard.ts <wallet> <program id>...
 */
import { Connection, PublicKey } from "@solana/web3.js";
import { deriveProgramDataPda } from "../client/pda";
import { fail, parsePubkey } from "./cli-utils";

/**
 * A program's upgrade authority, or null when the program is final. The
 * ProgramData layout: u32 account kind (3), u64 deployment slot, then the
 * authority as a Borsh Option<Pubkey> (tag byte, 32 bytes).
 */
export async function upgradeAuthority(connection: Connection, programId: PublicKey): Promise<PublicKey | null> {
  const programData = deriveProgramDataPda(programId);
  const account = await connection.getAccountInfo(programData);
  if (!account) {
    throw new Error(`${programId.toBase58()} has no ProgramData (${programData.toBase58()}): not an upgradeable program`);
  }
  const kind = account.data.readUInt32LE(0);
  if (kind !== 3) {
    throw new Error(`${programData.toBase58()} is loader account kind ${kind}, not ProgramData (3)`);
  }
  const tag = account.data[12];
  if (tag === 0) return null;
  if (tag !== 1) throw new Error(`${programData.toBase58()}: invalid authority option tag ${tag}`);
  return new PublicKey(account.data.subarray(13, 45));
}

/** Throw when `wallet` is the upgrade authority of any of `programIds`. */
export async function refuseUpgradeAuthorityWallet(
  connection: Connection,
  wallet: PublicKey,
  programIds: PublicKey[],
): Promise<void> {
  const owned: string[] = [];
  for (const id of programIds) {
    const authority = await upgradeAuthority(connection, id);
    if (authority?.equals(wallet)) owned.push(id.toBase58());
  }
  if (owned.length > 0) {
    throw new Error(
      `the test wallet ${wallet.toBase58()} is the upgrade authority (the owner) of ${owned.join(", ")}; ` +
        "a cluster test run must use a wallet that owns nothing under test",
    );
  }
}

async function main() {
  const [walletArg, ...programArgs] = process.argv.slice(2);
  if (!walletArg || programArgs.length === 0) fail("usage: cluster-test-guard.ts <wallet> <program id>...");
  const url = process.env.ANCHOR_PROVIDER_URL;
  if (!url) fail("Missing ANCHOR_PROVIDER_URL: the cluster's RPC endpoint");
  await refuseUpgradeAuthorityWallet(
    new Connection(url, "confirmed"),
    parsePubkey("wallet", walletArg),
    programArgs.map((p) => parsePubkey("program id", p)),
  );
  console.log("✅ The test wallet owns none of the programs under test.");
}

if (require.main === module) {
  main().catch((err) => {
    console.error(`❌ ${(err as Error).message}`);
    process.exit(1);
  });
}
