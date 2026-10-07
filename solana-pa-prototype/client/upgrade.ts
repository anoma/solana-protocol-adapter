/**
 * What the loader records about a deployed program: its upgrade authority,
 * and the identity of its code, the executable hash (sha256 of the code with
 * trailing zero bytes removed, as `solana-verify get-executable-hash` and
 * `get-program-hash` compute it and as the programs' `UpgradedEvent`
 * announces it).
 */
import { createHash } from "crypto";
import { Connection, PublicKey } from "@solana/web3.js";
import { deriveProgramDataPda } from "./pda";

/** Offset of the authority's option tag in a ProgramData account: after the u32 account kind and the u64 deployment slot. */
const PROGRAM_DATA_AUTHORITY_OFFSET = 12;
/** Bytes before the code in a ProgramData account: the loader's state enum, slot and optional authority. */
const PROGRAM_DATA_METADATA_LEN = PROGRAM_DATA_AUTHORITY_OFFSET + 1 + 32;
/** Bytes before the code in a loader buffer: the loader's state enum and optional authority. */
const BUFFER_METADATA_LEN = 37;

/** sha256 of `code` without its trailing zero bytes. */
export function executableHash(code: Buffer): Buffer {
  let end = code.length;
  while (end > 0 && code[end - 1] === 0) end--;
  return createHash("sha256").update(code.subarray(0, end)).digest();
}

/** The executable hash of the code a loader buffer's data holds. */
export function bufferExecutableHash(bufferData: Buffer): Buffer {
  return executableHash(bufferData.subarray(BUFFER_METADATA_LEN));
}

/** The executable hash of the code `programId` runs, read from its ProgramData. */
export async function deployedExecutableHash(connection: Connection, programId: PublicKey): Promise<Buffer> {
  const programData = await connection.getAccountInfo(deriveProgramDataPda(programId), "confirmed");
  if (!programData) throw new Error(`${programId.toBase58()} has no ProgramData account`);
  return executableHash(programData.data.subarray(PROGRAM_DATA_METADATA_LEN));
}

/** The code bytes `programId`'s ProgramData account holds room for: an upgrade's code must fit in it. */
export async function programDataCapacity(connection: Connection, programId: PublicKey): Promise<number> {
  const programData = await connection.getAccountInfo(deriveProgramDataPda(programId), "confirmed");
  if (!programData) throw new Error(`${programId.toBase58()} has no ProgramData account`);
  return programData.data.length - PROGRAM_DATA_METADATA_LEN;
}

/**
 * A program's upgrade authority, or null when the program is final. The
 * ProgramData layout: u32 account kind (3), u64 deployment slot, then the
 * authority as a Borsh Option<Pubkey> (tag byte, 32 bytes).
 */
export async function upgradeAuthority(connection: Connection, programId: PublicKey): Promise<PublicKey | null> {
  const programData = deriveProgramDataPda(programId);
  const account = await connection.getAccountInfo(programData);
  if (!account) {
    throw new Error(
      `${programId.toBase58()} has no ProgramData (${programData.toBase58()}): not an upgradeable program`,
    );
  }
  const kind = account.data.readUInt32LE(0);
  if (kind !== 3) {
    throw new Error(`${programData.toBase58()} is loader account kind ${kind}, not ProgramData (3)`);
  }
  const tag = account.data[PROGRAM_DATA_AUTHORITY_OFFSET];
  if (tag === 0) return null;
  if (tag !== 1) throw new Error(`${programData.toBase58()}: invalid authority option tag ${tag}`);
  return new PublicKey(account.data.subarray(PROGRAM_DATA_AUTHORITY_OFFSET + 1, PROGRAM_DATA_METADATA_LEN));
}
