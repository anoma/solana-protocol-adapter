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
  "77376b24d04a17480764c7fd2549aa023781e0d90c625a29fe220b5f4c015ed9",
  "hex",
);
