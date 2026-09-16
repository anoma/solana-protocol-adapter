/**
 * Create or extend a deployment's settlement lookup table. Run through
 * ops.sh, which sets the cluster and wallet:
 *
 *   ./scripts/dev.sh lookup-table --cluster <c> [--wallet <path>]
 *
 * The wallet pays and is the table's authority: it creates the table, and
 * only it can extend one it created. The key set comes from the deployed
 * programs: the adapter and its PAState (whose pinned router and selector
 * name the verifier entry, whose account names the verifier program), the
 * block-time forwarder, the SPL token forwarder, and for each mint in
 * STF_TOKEN_MINTS its escrow PDA and escrow ATA. A table entry need not
 * exist on chain, so the forwarder's keys go in before it is deployed and
 * a mint's before its escrow is created.
 *
 * Environment:
 *   PA_LOOKUP_TABLE  base58 address of the deployment's table, to extend it
 *                    with the keys it lacks. Omit to create a new table.
 *   STF_TOKEN_MINTS  comma-separated base58 mints (optional)
 *
 * Idempotent: with PA_LOOKUP_TABLE set, a rerun adds only missing keys and
 * sends nothing when there are none.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { BlockTimeForwarder } from "../target/types/block_time_forwarder";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { ensureSettlementLookupTable, settlementLookupKeys } from "../tests/utils/lookupTable";
import { derivePaStatePda } from "../tests/utils/pda";
import { requirePubkey } from "./cli-utils";
import { getVerifierEntryPda } from "./verifier-utils";

async function main() {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);
  const wallet = provider.wallet as anchor.Wallet;
  const adapter = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const forwarder = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
  const blockTimeForwarder = anchor.workspace.BlockTimeForwarder as Program<BlockTimeForwarder>;

  const [paState] = derivePaStatePda(adapter.programId);
  const state = await adapter.account.paStateAccount.fetch(paState);
  const proofSelector = Buffer.from(state.proofSelector);

  // The verifier program is whatever the router entry the deployment pinned
  // points at: VerifierEntry { selector: [u8; 4], verifier: Pubkey } after
  // the 8-byte Anchor discriminator.
  const [verifierEntry] = getVerifierEntryPda(proofSelector, state.verifierRouter);
  const entry = await provider.connection.getAccountInfo(verifierEntry);
  if (!entry) throw new Error(`verifier entry ${verifierEntry.toBase58()} does not exist on this cluster`);
  const verifierProgram = new PublicKey(entry.data.subarray(12, 44));

  const mints = (process.env.STF_TOKEN_MINTS ?? "")
    .split(",")
    .filter((s) => s.length > 0)
    .map((s) => new PublicKey(s));
  const existing = process.env.PA_LOOKUP_TABLE
    ? requirePubkey("PA_LOOKUP_TABLE", "the deployment's settlement lookup table, as a base58 pubkey")
    : undefined;

  const keys = settlementLookupKeys({
    paProgram: adapter.programId,
    verifierRouter: state.verifierRouter,
    proofSelector,
    verifierProgram,
    blockTimeForwarder: blockTimeForwarder.programId,
    splTokenForwarder: forwarder.programId,
    mints,
  });
  const { address, added, signature } = await ensureSettlementLookupTable(provider.connection, wallet.payer, keys, existing);

  console.log(`settlement lookup table: ${address.toBase58()} (authority ${wallet.publicKey.toBase58()})`);
  for (const key of keys) {
    console.log(`  ${added.some((a) => a.equals(key)) ? "+" : "="} ${key.toBase58()}`);
  }
  console.log(signature ? `${added.length} key(s) added in ${signature}` : "0 keys added, nothing sent");
  if (!existing) {
    console.log("Record the address in the cluster's deployment record and in anoma-pa-solana-client's SETTLE_LOOKUP_TABLE.");
  }
}

main().catch((err) => {
  console.error("❌ lookup-table failed:", err.message || err);
  process.exit(1);
});
