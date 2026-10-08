/**
 * Upgrade the protocol adapter, which owns its upgrades: the owner calls its
 * `upgrade` with a loader buffer it wrote, as pa-evm's owner calls
 * `upgradeToAndCall`. Run through ops.sh (`upgrade`), which writes the
 * buffer and sets the cluster and wallet; the wallet must be the owner.
 *
 *   npx ts-node -P tsconfig.json scripts/upgrade-program.ts path protocol_adapter
 *   npx ts-node -P tsconfig.json scripts/upgrade-program.ts upgrade protocol_adapter <buffer>
 *
 * `path` prints which upgrade path the program's upgrade authority leaves:
 * `program` when the authority is the program's own PDA (upgrade through
 * the program), `loader` when it is the wallet (the loader's own upgrade,
 * before `initialize` hands the authority over). Any
 * other authority, or none, is an error. `upgrade` verifies that the
 * program then runs the buffer's code and announced its hash.
 */
import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { upgradeAdapter } from "../client/instructions";
import { deriveUpgradeAuthorityPda } from "../client/pda";
import { bufferExecutableHash, deployedExecutableHash, upgradeAuthority } from "../client/upgrade";
import { cpiEventsOfSignature } from "../client/events";
import { fail, parsePubkey } from "./cli-utils";

async function main() {
  const [command, name, bufferArg] = process.argv.slice(2);
  if (name !== "protocol_adapter") fail(`${name} does not upgrade itself; only protocol_adapter does`);
  const provider = confirmedProvider();
  anchor.setProvider(provider);
  const program = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const wallet = provider.wallet.publicKey;

  if (command === "path") {
    const authority = await upgradeAuthority(provider.connection, program.programId);
    if (authority?.equals(deriveUpgradeAuthorityPda(program.programId))) console.log("program");
    else if (authority?.equals(wallet)) console.log("loader");
    else
      fail(
        `${name}'s upgrade authority is ${authority?.toBase58() ?? "none (final)"}, neither its PDA nor ${wallet.toBase58()}`,
      );
    return;
  }
  if (command !== "upgrade") fail(`usage: upgrade-program.ts <path|upgrade> <program> [buffer], got ${command}`);

  const buffer = parsePubkey("buffer", bufferArg);
  const bufferAccount = await provider.connection.getAccountInfo(buffer, "confirmed");
  if (!bufferAccount) fail(`buffer ${buffer.toBase58()} does not exist`);
  const expected = bufferExecutableHash(bufferAccount.data);

  console.log(`Upgrading ${name} (${program.programId.toBase58()}) from buffer ${buffer.toBase58()}`);
  const signature = await upgradeAdapter(program, wallet, buffer, wallet).rpc({ commitment: "confirmed" });

  const events = await cpiEventsOfSignature(provider.connection, program, signature);
  const upgraded = events.find((e) => e.name === "upgradedEvent");
  if (!upgraded) throw new Error(`transaction ${signature} landed without upgradedEvent`);
  const announced = Buffer.from(upgraded.data.executableHash as number[]);
  const deployed = await deployedExecutableHash(provider.connection, program.programId);
  if (!announced.equals(expected) || !deployed.equals(expected)) {
    throw new Error(
      `transaction ${signature}: the buffer's code hashes to ${expected.toString("hex")}, ` +
        `the event announced ${announced.toString("hex")}, the program runs ${deployed.toString("hex")}`,
    );
  }
  console.log(`✅ ${name} upgraded to ${expected.toString("hex")}. Signature: ${signature}`);
}

main().catch((err) => {
  console.error(`❌ upgrade-program failed:`, err);
  process.exit(1);
});
