/**
 * The only builders in this repository for instructions that change an
 * authority: the loader's SetAuthority on a program's ProgramData (move or
 * renounce the upgrade authority, which is the adapter's and the forwarder's
 * owner) and the forwarder's `set_emergency_caller`. They exist for the tests
 * and refuse any RPC endpoint that is not this machine's, so no repository
 * code can change an authority on devnet or mainnet; there, it is done by
 * hand (docs/OPERATIONS.md).
 */
import { Program } from "@anchor-lang/core";
import { Connection, PublicKey, TransactionInstruction } from "@solana/web3.js";
import { SplTokenForwarder } from "../../target/types/spl_token_forwarder";
import { BPF_LOADER_UPGRADEABLE, deriveProgramDataPda } from "../../client/pda";

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);

/** Throw unless `connection` talks to a validator on this machine. */
export function assertLocalValidator(connection: Connection): void {
  const host = new URL(connection.rpcEndpoint).hostname;
  if (!LOOPBACK_HOSTS.has(host)) {
    throw new Error(
      `authority instructions are built only against a local validator, not ${host}: ` +
        "on devnet and mainnet authority changes are made by hand",
    );
  }
}

/**
 * The loader's SetAuthority (instruction 4) on `programId`'s ProgramData,
 * signed by `current`: `next` becomes the upgrade authority, or none when
 * `next` is null (the program is final).
 */
export function localSetUpgradeAuthority(
  connection: Connection,
  programId: PublicKey,
  current: PublicKey,
  next: PublicKey | null,
): TransactionInstruction {
  assertLocalValidator(connection);
  return new TransactionInstruction({
    programId: BPF_LOADER_UPGRADEABLE,
    keys: [
      { pubkey: deriveProgramDataPda(programId), isSigner: false, isWritable: true },
      { pubkey: current, isSigner: true, isWritable: false },
      ...(next ? [{ pubkey: next, isSigner: false, isWritable: false }] : []),
    ],
    data: Buffer.from([4, 0, 0, 0]),
  });
}

/** `set_emergency_caller` by the committee; only while the adapter at `paState` is paused. */
export function localSetEmergencyCaller(
  connection: Connection,
  forwarder: Program<SplTokenForwarder>,
  committee: PublicKey,
  paState: PublicKey,
  caller: PublicKey,
) {
  assertLocalValidator(connection);
  return forwarder.methods.setEmergencyCaller(caller).accounts({ committee, paState });
}
