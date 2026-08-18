import { readFileSync } from "fs";
import path from "path";

export type TestMode = "real" | "mock";

// Selects which fixture set the suite runs against: tests/fixtures/ (real
// Groth16 proofs, selector 0x73c457ba) or tests/fixtures/mock/ (mock seals,
// selector 0xffffffff, accepted only by the localnet mock verifier). The
// fixture's selector drives everything downstream: the PA initialize
// argument, the verifier-entry PDA, and the verifier program account.
export const TEST_MODE: TestMode = (() => {
  const mode = process.env.PA_TEST_MODE ?? "real";
  if (mode !== "real" && mode !== "mock") {
    throw new Error(`PA_TEST_MODE must be "real" or "mock", got "${mode}"`);
  }
  return mode;
})();

export type Fixture = {
  format: string;
  aggregation_strategy: string;
  aggregation_proof_type: string;
  selector: string; // "0x73c457ba" format
  forwarder_type?: string;
  tx_b64: string;
  tx_tampered_b64?: string;
  consumed_nullifiers_b64: string[];
  historical_roots_b64?: string[];
};

export function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

export function loadFixture<T = Fixture>(filename: string): T {
  const dir =
    TEST_MODE === "mock"
      ? ["tests", "fixtures", "mock"]
      : ["tests", "fixtures"];
  return readJson<T>(path.resolve(process.cwd(), ...dir, filename));
}

export function parseSelectorFromFixture(selectorHex: string): Buffer {
  const hex = selectorHex.replace(/^0x/, "");
  if (hex.length !== 8) {
    throw new Error(`Invalid selector format: ${selectorHex} (expected 8 hex chars)`);
  }
  return Buffer.from(hex, "hex");
}
