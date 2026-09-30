import { readFileSync } from "fs";
import path from "path";
import { Ed25519Program, PublicKey } from "@solana/web3.js";

// PA_TEST_MODE selects which fixture set the suite runs against:
// tests/fixtures/ (real Groth16 proofs, selector 0x73c457ba) or
// tests/fixtures/mock/ (mock seals, selector 0xffffffff, accepted only by
// the localnet mock verifier). The mode's only job is picking the
// directory — the fixture's selector drives everything downstream: the PA
// initialize argument, the verifier-entry PDA, and the verifier program.
const FIXTURE_DIR: string = (() => {
  const mode = process.env.PA_TEST_MODE ?? "real";
  if (mode !== "real" && mode !== "mock") {
    throw new Error(`PA_TEST_MODE must be "real" or "mock", got "${mode}"`);
  }
  return mode === "mock"
    ? path.resolve(process.cwd(), "tests", "fixtures", "mock")
    : path.resolve(process.cwd(), "tests", "fixtures");
})();

// Replay data of an SPL forwarder wrap fixture (fixture-gen's
// SplTokenWrapMetadata). The actors are keypairs seeded with sha256 of a
// label (`seededKeypair`); the fixture carries the labels, not key material.
export type SplTokenWrapMetadata = {
  user_seed_label: string;
  mint_seed_label: string;
  amount: number;
  nonce: number;
  // The 44 bytes the user signed: base64 of the wrap message hash.
  signed_message_b64: string;
  signature_b64: string;
  logic_ref_b64: string;
};

export type SplTokenUnwrapMetadata = {
  mint_seed_label: string;
  amount: number;
  recipient_seed_label: string;
  logic_ref_b64: string;
};

export type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string; // "0x73c457ba" format
  tx_b64: string;
  tx_tampered_b64?: string;
  consumed_nullifiers_b64: string[];
  // Created commitments in instance order — the leaves settlement appends;
  // used to predict the produced-root marker. Absent on rejection fixtures.
  created_commitments_b64?: string[];
  historical_roots_b64?: string[];
  spl_token_wrap?: SplTokenWrapMetadata;
  spl_token_unwrap?: SplTokenUnwrapMetadata;
};

function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

export function loadFixture<T = Fixture>(filename: string): T {
  return readJson<T>(path.join(FIXTURE_DIR, filename));
}

/** Load a fixture, or fail naming the command that regenerates the fixture set. */
export function requireFixture(filename: string): Fixture {
  try {
    return loadFixture(filename);
  } catch (e: any) {
    const mode = process.env.PA_TEST_MODE ?? "real";
    throw new Error(`${filename} missing (${e.message}); generate with: ./scripts/dev.sh regen-fixtures ${mode}`);
  }
}

export function parseSelectorFromFixture(selectorHex: string): Buffer {
  const hex = selectorHex.replace(/^0x/, "");
  if (hex.length !== 8) {
    throw new Error(`Invalid selector format: ${selectorHex} (expected 8 hex chars)`);
  }
  return Buffer.from(hex, "hex");
}

/** The created commitments a successful settlement of `fixture` appends. */
export function createdCommitmentsOf(fixture: Fixture): Buffer[] {
  if (!fixture.created_commitments_b64?.length) {
    throw new Error("fixture is missing created_commitments_b64 — regenerate it with ./scripts/dev.sh regen-fixtures");
  }
  return fixture.created_commitments_b64.map((b) => Buffer.from(b, "base64"));
}

/** The serialized tampered clone of `fixture`'s transaction. */
export function tamperedTxOf(fixture: Fixture): Buffer {
  if (!fixture.tx_tampered_b64) {
    throw new Error("fixture is missing tx_tampered_b64 — regenerate it with ./scripts/dev.sh regen-fixtures");
  }
  return Buffer.from(fixture.tx_tampered_b64, "base64");
}

/** The ed25519 instruction carrying a wrap fixture's signature, by `signer`, over its signed message. */
export function wrapAuthorizationIx(signer: PublicKey, fixture: Fixture) {
  return Ed25519Program.createInstructionWithPublicKey({
    publicKey: signer.toBytes(),
    message: Buffer.from(fixture.spl_token_wrap!.signed_message_b64, "base64"),
    signature: Buffer.from(fixture.spl_token_wrap!.signature_b64, "base64"),
  });
}
