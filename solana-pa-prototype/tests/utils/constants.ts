import testForwarderIdl from "../../target/idl/test_forwarder.json";
import { idlNumber } from "../../client/constants";

// The test forwarder's mode that logs `input[1]` lines of 100 bytes.
export const TEST_FORWARDER_MODE_LOG = idlNumber(testForwarderIdl, "MODE_LOG");

// The empty-tree root at the initial depth: arm-risc0's PADDING_LEAF, a
// Digest the IDL cannot carry.
export const EMPTY_TREE_ROOT_INITIAL = Buffer.from(
  "cc1d2f838445db7aec431df9ee8a871f40e7aa5e064fc056633ef8c60fab7b06",
  "hex",
);

// The workspace build the suite deploys, which the upgrade tests write into
// loader buffers.
export const ADAPTER_SO = "target/deploy/protocol_adapter.so";
