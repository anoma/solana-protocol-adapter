// The operator scripts' argument parsing (scripts/cli-utils.ts): every value
// an operator passes to a governance command reaches the program through
// these, so a value they misread would be sent to a live cluster.
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { parsePubkey, requireEnv, requireHexBytes, requirePubkey, requireRawAmount } from "../scripts/cli-utils";

const VAR = "CLI_UTILS_TEST_VALUE";

/** Run `parse` with VAR set to `value` (unset when undefined), restoring the environment after. */
function withEnv<T>(value: string | undefined, parse: () => T): T {
  const previous = process.env[VAR];
  if (value === undefined) delete process.env[VAR];
  else process.env[VAR] = value;
  try {
    return parse();
  } finally {
    if (previous === undefined) delete process.env[VAR];
    else process.env[VAR] = previous;
  }
}

describe("operator script arguments (scripts/cli-utils.ts)", () => {
  it("requireEnv rejects a missing or empty variable, naming it and what it is", () => {
    assert.throws(() => withEnv(undefined, () => requireEnv(VAR, "the thing")), `Missing ${VAR}: the thing`);
    assert.throws(() => withEnv("", () => requireEnv(VAR, "the thing")), `Missing ${VAR}: the thing`);
    assert.equal(
      withEnv("x", () => requireEnv(VAR, "the thing")),
      "x",
    );
  });

  it("requireHexBytes reads exactly the requested width, with or without 0x", () => {
    const bytes = Array.from({ length: 32 }, (_, i) => i);
    const hex = Buffer.from(bytes).toString("hex");
    assert.deepEqual(
      withEnv(hex, () => requireHexBytes(VAR, 32, "")),
      bytes,
    );
    assert.deepEqual(
      withEnv(`0x${hex.toUpperCase()}`, () => requireHexBytes(VAR, 32, "")),
      bytes,
    );
    assert.deepEqual(
      withEnv("0x73c457ba", () => requireHexBytes(VAR, 4, "")),
      [0x73, 0xc4, 0x57, 0xba],
    );
  });

  it("requireHexBytes rejects the wrong width and non-hex input instead of truncating or padding", () => {
    for (const bad of ["abcd", "00".repeat(31), "00".repeat(33), "0g".repeat(32), `${"00".repeat(31)}0`]) {
      assert.throws(() => withEnv(bad, () => requireHexBytes(VAR, 32, "")), `${VAR} must be 64 hex chars (32 bytes)`);
    }
  });

  it("requirePubkey reads a base58 pubkey and rejects anything else", () => {
    const key = Keypair.generate().publicKey;
    assert.ok(withEnv(key.toBase58(), () => requirePubkey(VAR, "")).equals(key));
    assert.throws(() => withEnv("not-a-key", () => requirePubkey(VAR, "")), `${VAR} is not a valid pubkey`);
  });

  it("parsePubkey reads one element of a list variable and names the variable when it is not a pubkey", () => {
    const key = Keypair.generate().publicKey;
    assert.ok(parsePubkey("LIST", key.toBase58()).equals(key));
    assert.throws(() => parsePubkey("LIST", "not-a-key"), `LIST is not a valid pubkey: "not-a-key"`);
  });

  it("requireRawAmount reads a non-negative integer exactly, beyond 2^53", () => {
    assert.equal(
      withEnv("0", () => requireRawAmount(VAR, "")),
      0n,
    );
    assert.equal(
      withEnv("18446744073709551615", () => requireRawAmount(VAR, "")),
      18446744073709551615n,
    );
    for (const bad of ["-1", "1.5", "1e6", " 1", "0x10"]) {
      assert.throws(() => withEnv(bad, () => requireRawAmount(VAR, "")), `${VAR} must be a non-negative integer`);
    }
  });
});
