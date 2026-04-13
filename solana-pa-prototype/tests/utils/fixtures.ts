import { readFileSync } from "fs";
import path from "path";

export function readJson<T>(filePath: string): T {
  return JSON.parse(readFileSync(filePath, "utf8")) as T;
}

export function loadFixture<T = Fixture>(filename: string): T {
  return readJson<T>(path.resolve(process.cwd(), "tests", "fixtures", filename));
}

export function parseSelectorFromFixture(selectorHex: string): Buffer {
  const hex = selectorHex.replace(/^0x/, "");
  if (hex.length !== 8) {
    throw new Error(`Invalid selector format: ${selectorHex} (expected 8 hex chars)`);
  }
  return Buffer.from(hex, "hex");
}

// Fixture types

export type SplTokenWrapMetadata = {
  user_secret_key_b64: string;
  user_pubkey_b64: string;
  mint_seed_b64: string;
  token_mint_b58: string;
  amount: number;
  nonce: number;
  deadline: number;
  action_tree_root_b64: string;
  signature_b64: string;
  logic_ref_b64: string;
};

export type SplTokenUnwrapMetadata = {
  mint_seed_b64: string;
  token_mint_b58: string;
  amount: number;
  recipient_seed_b64: string;
  recipient_b58: string;
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
  spl_token_wrap?: SplTokenWrapMetadata;
  spl_token_unwrap?: SplTokenUnwrapMetadata;
};
