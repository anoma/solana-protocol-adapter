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

// fixture-gen's RECIPIENT_SEED_LABEL: the recipient its unwraps release to
// and its relay fixture names, needed before any unwrap is proven.
export const UNWRAP_RECIPIENT_SEED_LABEL = "spl_token_forwarder_test_recipient";

// The commitment anoma/risc0-kind-tables publishes for solana-devnet
// (data/generated/staging/commitments.json): the table fixture-gen's
// kind_table_solana_devnet.json holds, which spl_token_wrap_devnet_kind_table
// is proven against.
export const SOLANA_DEVNET_KIND_TABLE_COMMITMENT = Buffer.from(
  "0a362a8f9b49d3ec7bf75f7f30c89f7231ab164e50835bf1b14f0e35328c7c69",
  "hex",
);

// The workspace builds the suite deploys, which the upgrade tests write into
// loader buffers.
export const ADAPTER_SO = "target/deploy/protocol_adapter.so";
export const FORWARDER_SO = "target/deploy/spl_token_forwarder.so";
