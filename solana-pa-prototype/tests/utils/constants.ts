// The empty-tree root at the initial depth: arm-risc0's PADDING_LEAF, a
// Digest the IDL cannot carry.
export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex",
);

// fixture-gen's RECIPIENT_SEED_LABEL: the recipient its unwraps release to
// and its relay fixture names, needed before any unwrap is proven.
export const UNWRAP_RECIPIENT_SEED_LABEL = "spl_token_forwarder_test_recipient";

// The commitment anoma/risc0-kind-tables publishes for solana-devnet
// (data/generated/staging/commitments.json): the table fixture-gen's
// kind_table_solana_devnet.json holds, which spl_token_wrap_devnet_kind_table
// is proven against.
export const SOLANA_DEVNET_KIND_TABLE_COMMITMENT = Buffer.from(
  "e6b8e1832b61d0d6ed3685ee46e62a8f4723a49df520096c5d96ff509263b483",
  "hex",
);
