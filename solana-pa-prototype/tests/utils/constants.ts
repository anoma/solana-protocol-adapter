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
