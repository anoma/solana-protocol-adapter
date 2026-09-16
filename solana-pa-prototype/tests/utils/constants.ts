export const PA_STATE_SEED = Buffer.from("pa_state");
export const NULLIFIER_SEED = Buffer.from("nullifier");
export const TX_DATA_SEED = Buffer.from("tx_data");
export const ROOT_MARKER_SEED = Buffer.from("root");

export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex"
);

// Commitment of the empty kind table (sha256 of zero bytes). Every fixture
// commits to the empty table (fixture-gen's committed kind_table.json), so
// the test deployment pins the same commitment at initialize.
export const EMPTY_KIND_TABLE_COMMITMENT = Buffer.from(
  "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  "hex"
);

// Protocol constants matching Rust defaults (from state.rs)
export const MIN_EXPIRY_SLOTS = 100;
export const MAX_EXPIRY_SLOTS = 216_000;
// 7 days at 400ms/slot — matches SEVEN_DAYS_SLOTS in state.rs
export const SEVEN_DAYS_SLOTS = 1_512_000;

// SPL token forwarder: PDA seeds and layout (programs/spl-token-forwarder/src/state.rs)
export const CONFIG_SEED = Buffer.from("config");
export const ESCROW_SEED = Buffer.from("escrow");
export const NONCE_BITMAP_SEED = Buffer.from("nonce_bitmap");
export const NONCES_PER_WORD = 256n;
// A nonce bitmap account is the raw 32-byte bitmap.
export const NONCE_BITMAP_ACCOUNT_SIZE = 32;
export const NONCE_BITMAP_DATA_OFFSET = 0;
// forward_call operation codes (programs/spl-token-forwarder/src/lib.rs)
export const OP_WRAP = 0;
export const OP_UNWRAP = 1;
export const OP_EMERGENCY_WITHDRAW = 0;

// Deterministic keypair labels shared across the forwarder test files: 01
// initializes the config with this committee, zz and zzz act as it.
export const EMERGENCY_COMMITTEE_LABEL = "emergency_committee_seed";
export const EMERGENCY_CALLER_LABEL = "emergency_caller_seed";

// Anchor constraint error patterns for assertion matching
export const AUTHORITY_MISMATCH_PATTERN = /Unauthorized|has.?one.*constraint.*violated|ConstraintHasOne/i;
export const SEED_MISMATCH_PATTERN = /ConstraintSeeds|ConstraintHasOne|has.?one|seeds constraint|Unauthorized/i;
export const ADDRESS_MISMATCH_PATTERN = /ConstraintAddress|address constraint/i;
