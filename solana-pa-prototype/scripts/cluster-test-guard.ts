/**
 * Refuse a cluster test run whose wallet owns a deployment under test.
 *
 * The cluster test run (`ops.sh test --cluster <c>`) signs with its wallet
 * against a live deployment. A wallet that holds an owner's role could have
 * a spec pause the adapter, replace its kind table, upgrade a program, or
 * renounce the ownership for good. The roles: each program's upgrade
 * authority while it is not the program's own upgrade authority PDA, and
 * the adapter's stored owner. Run through ops.sh before any spec:
 *
 *   npx ts-node -P tsconfig.json scripts/cluster-test-guard.ts <program id>...
 */
import * as anchor from "@anchor-lang/core";
import { AnchorProvider, Program } from "@anchor-lang/core";
import { Connection, PublicKey } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { derivePaStatePda, deriveUpgradeAuthorityPda } from "../client/pda";
import { upgradeAuthority } from "../client/upgrade";
import { fail, parsePubkey } from "./cli-utils";

/** A key with an owner's role, and the role, for the refusal message. */
export type OwnerRole = { role: string; key: PublicKey };

/**
 * Every owner's role in the deployment: each of `programIds`' upgrade
 * authority unless it is the program's own PDA (or the program is final),
 * and the adapter's stored owner once it is initialized.
 */
export async function ownerRoles(
  connection: Connection,
  programIds: PublicKey[],
  adapter: Program<ProtocolAdapter>,
): Promise<OwnerRole[]> {
  const roles: OwnerRole[] = [];
  for (const id of programIds) {
    const authority = await upgradeAuthority(connection, id);
    if (authority && !authority.equals(deriveUpgradeAuthorityPda(id))) {
      roles.push({ role: `the upgrade authority of ${id.toBase58()}`, key: authority });
    }
  }
  const state = await adapter.account.paStateAccount.fetchNullable(derivePaStatePda(adapter.programId)[0]);
  if (state) roles.push({ role: `the owner of the adapter ${adapter.programId.toBase58()}`, key: state.owner });
  return roles;
}

/** Throw when `wallet` holds any of `roles`, naming each. */
export function refuseOwnerWallet(wallet: PublicKey, roles: OwnerRole[]): void {
  const held = roles.filter((r) => r.key.equals(wallet)).map((r) => r.role);
  if (held.length > 0) {
    throw new Error(
      `the test wallet ${wallet.toBase58()} is ${held.join(" and ")}; ` +
        "a cluster test run must use a wallet that owns nothing under test",
    );
  }
}

async function main() {
  const programArgs = process.argv.slice(2);
  if (programArgs.length === 0) fail("usage: cluster-test-guard.ts <program id>...");
  const provider = AnchorProvider.env();
  anchor.setProvider(provider);
  const adapter = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  refuseOwnerWallet(
    provider.wallet.publicKey,
    await ownerRoles(
      provider.connection,
      programArgs.map((p) => parsePubkey("program id", p)),
      adapter,
    ),
  );
  console.log("✅ The test wallet owns none of the programs under test.");
}

if (require.main === module) {
  main().catch((err) => {
    console.error(`❌ ${(err as Error).message}`);
    process.exit(1);
  });
}
