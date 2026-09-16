import { readFileSync } from "fs";
import path from "path";

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

// Replay data of an SPL forwarder wrap fixture (fixture-gen's SplTokenWrapMetadata).
export type SplTokenWrapMetadata = {
  user_secret_key_b64: string;
  mint_seed_b64: string;
  amount: number;
  nonce: number;
  // The 44 bytes the user signed: base64 of the wrap message hash.
  signed_message_b64: string;
  signature_b64: string;
  logic_ref_b64: string;
};

export type SplTokenUnwrapMetadata = {
  mint_seed_b64: string;
  amount: number;
  recipient_seed_b64: string;
  logic_ref_b64: string;
};

export type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string; // "0x73c457ba" format
  forwarder_type?: string;
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

export function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

export function loadFixture<T = Fixture>(filename: string): T {
  return readJson<T>(path.join(FIXTURE_DIR, filename));
}

/** Load a fixture, or fail naming the fixture-gen command that produces it. */
export function requireFixture(filename: string, flags: string): Fixture {
  try {
    return loadFixture(filename);
  } catch (e: any) {
    const mock = process.env.PA_TEST_MODE === "mock" ? "--mock " : "";
    const target = path.relative(process.cwd(), path.join(FIXTURE_DIR, filename));
    throw new Error(
      `${filename} missing (${e.message}); generate with: ./scripts/dev.sh gen-fixtures ${mock}${flags} ${target}`
    );
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
    throw new Error("fixture is missing created_commitments_b64 — regenerate or refresh-fields it");
  }
  return fixture.created_commitments_b64.map((b) => Buffer.from(b, "base64"));
}
