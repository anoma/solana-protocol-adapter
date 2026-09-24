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

export function requirePubkey(name: string, what: string): PublicKey {
  const raw = requireEnv(name, what);
  try {
    return new PublicKey(raw);
  } catch {
    throw new Error(`${name} is not a valid pubkey: "${raw}"`);
  }
}

/** A fixed-width hex byte string (an optional 0x prefix is accepted), as the byte array Anchor takes. */
export function requireHexBytes(name: string, byteLen: number, what: string): number[] {
  const hex = requireEnv(name, what).replace(/^0x/, "");
  if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== byteLen * 2) {
    throw new Error(`${name} must be ${byteLen * 2} hex chars (${byteLen} bytes), got "${hex}"`);
  }
  return Array.from(Buffer.from(hex, "hex"));
}

/** A token amount in raw units: a non-negative integer. */
export function requireRawAmount(name: string, what: string): bigint {
  const raw = requireEnv(name, what);
  if (!/^\d+$/.test(raw)) throw new Error(`${name} must be a non-negative integer, got "${raw}"`);
  return BigInt(raw);
}
