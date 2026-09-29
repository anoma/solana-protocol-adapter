import blockTimeForwarderIdl from "../../target/idl/block_time_forwarder.json";
import paIdl from "../../target/idl/protocol_adapter.json";
import forwarderIdl from "../../target/idl/spl_token_forwarder.json";

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

// The empty-tree root at the initial depth: arm-risc0's PADDING_LEAF, a
// Digest the IDL cannot carry.
export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex",
);

// The commitment anoma/risc0-kind-tables publishes for solana-devnet
// (data/generated/staging/commitments.json): the table fixture-gen's
// kind_table_solana_devnet.json holds, which spl_token_wrap_devnet_kind_table
// is proven against.
export const SOLANA_DEVNET_KIND_TABLE_COMMITMENT = Buffer.from(
  "f7205e227c4bbb3cf3c4a5228b806ec9aab3bb1926063543dff98169df4b68a5",
  "hex",
);

// Anchor constraint error patterns for assertion matching
export const AUTHORITY_MISMATCH_PATTERN = /Unauthorized|has.?one.*constraint.*violated|ConstraintHasOne/i;
export const SEED_MISMATCH_PATTERN = /ConstraintSeeds|ConstraintHasOne|has.?one|seeds constraint|Unauthorized/i;
export const ADDRESS_MISMATCH_PATTERN = /ConstraintAddress|address constraint/i;
