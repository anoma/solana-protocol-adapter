/**
 * Shared argument handling for the operator scripts: every parameter comes
 * from an environment variable, is validated up front, and a missing or
 * malformed one throws an error saying what it is and why nothing is
 * guessed. Each script's top-level catch prints it and exits non-zero.
 */
import { PublicKey } from "@solana/web3.js";

export function fail(message: string): never {
  console.error(`❌ ${message}`);
  process.exit(1);
}

export function requireEnv(name: string, what: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}: ${what}`);
  return value;
}

/** `raw`, a value of variable `name`, as a base58 pubkey. */
export function parsePubkey(name: string, raw: string): PublicKey {
  try {
    return new PublicKey(raw);
  } catch {
    throw new Error(`${name} is not a valid pubkey: "${raw}"`);
  }
}

export function requirePubkey(name: string, what: string): PublicKey {
  return parsePubkey(name, requireEnv(name, what));
}

/** `raw`, a value of variable `name`, as a fixed-width hex byte string (an optional 0x prefix is accepted), as the byte array Anchor takes. */
export function parseHexBytes(name: string, raw: string, byteLen: number): number[] {
  const hex = raw.replace(/^0x/, "");
  if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== byteLen * 2) {
    throw new Error(`${name} must be ${byteLen * 2} hex chars (${byteLen} bytes), got "${hex}"`);
  }
  return Array.from(Buffer.from(hex, "hex"));
}

export function requireHexBytes(name: string, byteLen: number, what: string): number[] {
  return parseHexBytes(name, requireEnv(name, what), byteLen);
}
