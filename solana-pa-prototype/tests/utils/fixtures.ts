import { spawn } from "child_process";
import { existsSync, readFileSync } from "fs";
import path from "path";
import { Ed25519Program, PublicKey } from "@solana/web3.js";

// PA_TEST_MODE selects which fixture set the suite runs against:
// tests/fixtures/ (real Groth16 proofs, selector 0x73c457ba) or
// tests/fixtures/mock/ (mock seals, selector 0xffffffff, accepted only by
// the localnet mock verifier). The mode's only job is picking the
// directory — the fixture's selector drives everything downstream: the PA
// initialize argument, the verifier-entry PDA, and the verifier program.
// A cluster run (ops.sh test) proves its own set for the deployment into
// PA_FIXTURE_DIR, each fixture when a test first loads it, under the run's
// salt (PA_FIXTURE_SALT) and the deployment's kind table (PA_KIND_TABLE).
const MODE = process.env.PA_TEST_MODE ?? "real";
if (MODE !== "real" && MODE !== "mock") {
  throw new Error(`PA_TEST_MODE must be "real" or "mock", got "${MODE}"`);
}
const FIXTURE_DIR: string = process.env.PA_FIXTURE_DIR
  ? path.resolve(process.env.PA_FIXTURE_DIR)
  : MODE === "mock"
    ? path.resolve(process.cwd(), "tests", "fixtures", "mock")
    : path.resolve(process.cwd(), "tests", "fixtures");

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
  /** Absent when the unwrap releases to the forwarder's escrow authority. */
  recipient_seed_label?: string;
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

/**
 * Load a fixture of the suite's set. In a cluster run, a fixture the run's
 * set does not hold yet is proven first: proving each fixture when a test
 * first needs it stops at the first failing test and proves nothing the run
 * does not use.
 */
export async function loadFixture<T = Fixture>(filename: string): Promise<T> {
  const file = path.join(FIXTURE_DIR, filename);
  if (!existsSync(file) && process.env.PA_FIXTURE_SALT) await proveForClusterRun(filename);
  return readJson<T>(file);
}

/**
 * Prove `filename` into a cluster run's fixture set with the recipe
 * scripts/regen-fixtures.sh holds for it. The proof takes minutes, so it runs
 * without blocking the event loop: a process that blocks that long leaves
 * its pooled RPC connections stale, and its next request fails.
 */
function proveForClusterRun(filename: string): Promise<void> {
  const kindTable = process.env.PA_KIND_TABLE;
  if (!kindTable) throw new Error("a cluster run (PA_FIXTURE_SALT set) needs PA_KIND_TABLE to prove its fixtures");
  const args = ["real", "--out", FIXTURE_DIR, "--salt", process.env.PA_FIXTURE_SALT!, "--kind-table", kindTable];
  return new Promise((resolve, reject) => {
    const prover = spawn("./scripts/regen-fixtures.sh", [...args, "--only", filename], { stdio: "inherit" });
    prover.on("error", reject);
    prover.on("exit", (code, signal) =>
      code === 0 ? resolve() : reject(new Error(`proving ${filename} failed (exit ${code}, signal ${signal})`)),
    );
  });
}

/**
 * Read a fixture that must already exist, for a file whose fixtures are all
 * committed, or before any test runs; fail naming the command that
 * regenerates the fixture set.
 */
export function requireFixture(filename: string): Fixture {
  const file = path.join(FIXTURE_DIR, filename);
  if (!existsSync(file)) {
    const mode = process.env.PA_TEST_MODE ?? "real";
    throw new Error(`${file} missing; generate with: ./scripts/dev.sh regen-fixtures ${mode}`);
  }
  return readJson<Fixture>(file);
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
  if (fixture.created_commitments_b64 === undefined) {
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
