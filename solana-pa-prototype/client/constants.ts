import blockTimeForwarderIdl from "../target/idl/block_time_forwarder.json";
import paIdl from "../target/idl/protocol_adapter.json";
import forwarderIdl from "../target/idl/spl_token_forwarder.json";

type IdlWithConstants = { metadata: { name: string }; constants: { name: string; value: string }[] };

// A program's `#[constant]`, as its IDL renders it: Rust's `{:?}` of the value.
function idlConstant(idl: IdlWithConstants, name: string): string {
  const constant = idl.constants.find((c) => c.name === name);
  if (!constant) {
    throw new Error(`The ${idl.metadata.name} IDL has no constant ${name}: rebuild the IDLs (anchor idl build).`);
  }
  return constant.value;
}

// A byte-string or byte-array constant, rendered as a list of numbers.
function idlBytes(idl: IdlWithConstants, name: string): Buffer {
  return Buffer.from(JSON.parse(idlConstant(idl, name)) as number[]);
}

function idlNumber(idl: IdlWithConstants, name: string): number {
  const value = Number(idlConstant(idl, name));
  if (!Number.isSafeInteger(value)) {
    throw new Error(`The ${idl.metadata.name} IDL constant ${name} is not a safe integer.`);
  }
  return value;
}

// Protocol adapter
export const PA_STATE_SEED = idlBytes(paIdl, "PA_STATE_SEED");
export const NULLIFIER_SEED = idlBytes(paIdl, "NULLIFIER_SEED");
export const TX_DATA_SEED = idlBytes(paIdl, "TX_DATA_SEED");
export const ROOT_SEED = idlBytes(paIdl, "ROOT_SEED");
export const SCHEMA_VERSION = idlNumber(paIdl, "SCHEMA_VERSION");
export const EMPTY_KIND_TABLE_COMMITMENT = idlBytes(paIdl, "EMPTY_KIND_TABLE_COMMITMENT");
export const MIN_ALLOWED_EXPIRY = idlNumber(paIdl, "MIN_ALLOWED_EXPIRY");
export const MIN_EXPIRY_SLOTS = idlNumber(paIdl, "MIN_EXPIRY_SLOTS");
export const MAX_EXPIRY_SLOTS = idlNumber(paIdl, "MAX_EXPIRY_SLOTS");
export const SEVEN_DAYS_SLOTS = idlNumber(paIdl, "SEVEN_DAYS_SLOTS");

// SPL token forwarder
export const CONFIG_SEED = idlBytes(forwarderIdl, "CONFIG_SEED");
export const ESCROW_SEED = idlBytes(forwarderIdl, "ESCROW_SEED");
export const NONCE_BITMAP_SEED = idlBytes(forwarderIdl, "NONCE_BITMAP_SEED");
export const NONCES_PER_WORD = BigInt(idlNumber(forwarderIdl, "NONCES_PER_WORD"));
export const OP_UNWRAP = idlNumber(forwarderIdl, "OP_UNWRAP");

// Block-time forwarder
export const RESULT_LT = idlNumber(blockTimeForwarderIdl, "RESULT_LT");
