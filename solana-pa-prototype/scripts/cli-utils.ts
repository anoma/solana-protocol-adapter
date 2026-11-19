/**
 * Shared entry and argument handling for the operator scripts: every
 * parameter comes from an environment variable, is validated up front, and a
 * missing or malformed one throws an error saying what it is and why nothing
 * is guessed. `runScript` prints it and exits non-zero.
 */
import { PublicKey } from "@solana/web3.js";

/**
 * Run a script's `main` and exit with its outcome: 0 once it resolves, 1 with
 * "❌ <failure>:" and the error once it rejects. The exit is explicit because
 * a finished script must not wait for open handles: web3.js reconnects its
 * RPC websocket whenever the endpoint closes it, which kept a script alive
 * for hours after its transaction landed.
 */
export function runScript(failure: string, main: () => Promise<void>): void {
  main().then(
    () => exitFlushed(0),
    (err) => {
      console.error(`❌ ${failure}:`, err);
      exitFlushed(1);
    },
  );
}

// process.exit drops output still queued on a pipe (pipes are asynchronous on
// macOS); an empty write's callback runs once the earlier writes are flushed.
function exitFlushed(code: number): void {
  process.stdout.write("", () => process.stderr.write("", () => process.exit(code)));
}

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
