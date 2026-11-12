// The root of the empty commitment tree and of the empty kind tree:
// arm-risc0's PADDING_LEAF.
export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex",
);

// The workspace build the suite deploys, which the upgrade tests write into
// loader buffers.
export const ADAPTER_SO = "target/deploy/protocol_adapter.so";
