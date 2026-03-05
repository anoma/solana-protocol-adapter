import { PublicKey } from "@solana/web3.js";

// PDA seeds
export const PA_STATE_SEED = Buffer.from("pa_state");
export const NULLIFIER_SEED = Buffer.from("nullifier");
export const TX_DATA_SEED = Buffer.from("tx_data");
export const ROOT_MARKER_SEED = Buffer.from("root");
export const CONFIG_SEED = Buffer.from("config");
export const ESCROW_SEED = Buffer.from("escrow");
export const NONCE_BITMAP_SEED = Buffer.from("nonce_bitmap");

// Nonce bitmap layout: each PDA stores 256 nonces (one u256 word)
export const NONCES_PER_WORD = 256n;

// Genesis root for depth-1 tree
export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex"
);

// Program IDs
export const SPL_TOKEN_FORWARDER_PROGRAM_ID = new PublicKey("6cMwWUEoTnj8ManPCwAtXw5vdnp16mQKfUTdbxLNszN1");

// SPL Token Forwarder operation codes
export const OP_WRAP = 0;
export const OP_UNWRAP = 1;

// Protocol constants matching Rust defaults (from state.rs)
export const MIN_EXPIRY_SLOTS = 100;
export const MAX_EXPIRY_SLOTS = 216_000;
// 7 days at 400ms/slot — matches SEVEN_DAYS_SLOTS in state.rs
export const SEVEN_DAYS_SLOTS = 1_512_000;

// Anchor constraint error patterns for assertion matching
export const AUTHORITY_MISMATCH_PATTERN = /Unauthorized|has.?one.*constraint.*violated|ConstraintHasOne/i;
export const SEED_MISMATCH_PATTERN = /ConstraintSeeds|ConstraintHasOne|has.?one|seeds constraint|Unauthorized/i;
export const ADDRESS_MISMATCH_PATTERN = /ConstraintAddress|address constraint/i;
