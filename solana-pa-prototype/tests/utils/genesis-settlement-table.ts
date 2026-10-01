/**
 * Write the suite's settlement lookup table as a solana-test-validator
 * genesis account, into the directory given as the only argument.
 *
 * A table created on a running validator serves its new keys only once the
 * slot that added them is rooted, about 32 slots later: a v0 transaction
 * naming them before then is dropped. Every spec file runs on a fresh
 * validator, so creating the table in each would wait that long in each.
 * The table's keys are fixed by the deployment, so the validator starts
 * with it instead, extended at slot 0, and is warped to slot 1 so that slot
 * is already rooted (anchor-test.sh).
 *
 * Run by anchor-test.sh with the suite's environment (ANCHOR_WALLET,
 * ANCHOR_PROVIDER_URL, PA_TEST_MODE), which fixes the verifier the keys name.
 */
import { AddressLookupTableAccount, AddressLookupTableProgram, PublicKey } from "@solana/web3.js";
import { writeGenesisAccountFixtures } from "../../scripts/genesis-account";
import { provider, suiteSettlementKeys } from "./adapterSuite";

const args = process.argv.slice(2);
if (args.length !== 1) {
  throw new Error("usage: genesis-settlement-table.ts <output directory>");
}

const U64_MAX = 0xffffffffffffffffn;

/**
 * The address lookup table program's account: ProgramState::LookupTable
 * (u32 tag 1), then LookupTableMeta { deactivation_slot, last_extended_slot,
 * last_extended_slot_start_index, authority: Option<Pubkey>, padding: u16 }
 * (56 bytes), then the addresses: a table never deactivated, extended at
 * slot 0 from index 0.
 */
function lookupTableData(authority: PublicKey, addresses: PublicKey[]): Buffer {
  const meta = Buffer.alloc(56);
  meta.writeUInt32LE(1, 0);
  meta.writeBigUInt64LE(U64_MAX, 4);
  meta.writeBigUInt64LE(0n, 12);
  meta.writeUInt8(0, 20);
  meta.writeUInt8(1, 21);
  authority.toBuffer().copy(meta, 22);
  return Buffer.concat([meta, ...addresses.map((a) => a.toBuffer())]);
}

// Rent exemption under the default rent: 128 bytes of account overhead plus
// the data, at 3,480 lamports per byte-year, for two years.
const rentExempt = (size: number) => (128 + size) * 3480 * 2;

const authority = provider.wallet.publicKey;
const addresses = suiteSettlementKeys([]);
const data = lookupTableData(authority, addresses);

// The encoding must be what web3.js's lookup-table decoder reads back.
const decoded = new AddressLookupTableAccount({
  key: PublicKey.default,
  state: AddressLookupTableAccount.deserialize(data),
});
if (
  !decoded.isActive() ||
  !decoded.state.authority?.equals(authority) ||
  decoded.state.addresses.length !== addresses.length ||
  !decoded.state.addresses.every((a, i) => a.equals(addresses[i]))
) {
  throw new Error("the genesis settlement table does not decode to its keys");
}

// The address a table created by the wallet at slot 0 would take.
const [, address] = AddressLookupTableProgram.createLookupTable({ authority, payer: authority, recentSlot: 0 });

writeGenesisAccountFixtures({
  outDir: args[0],
  prefix: "settlement-table-",
  owner: AddressLookupTableProgram.programId,
  lamports: rentExempt(data.length),
  accounts: [{ pubkey: address, data }],
});
