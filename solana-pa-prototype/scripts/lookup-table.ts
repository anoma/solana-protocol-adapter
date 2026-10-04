/**
 * Create or extend a deployment's settlement lookup table. Run through
 * ops.sh, which sets the cluster and wallet:
 *
 *   ./scripts/dev.sh lookup-table --cluster <c> [--wallet <path>]
 *
 * The key set is `settlementLookupKeys` (client/lookupTable.ts), fed
 * by the deployed programs and PAState's pinned router and selector; see
 * docs/OPERATIONS.md, "The settlement lookup table". The wallet pays and is
 * the table's authority.
 *
 * Environment:
 *   PA_LOOKUP_TABLE  base58 address of the deployment's table, to extend it
 *                    with the keys it lacks. Omit to create a new table.
 */
import * as anchor from "@anchor-lang/core";
import { confirmedProvider } from "../client/provider";
import { Program } from "@anchor-lang/core";
import { BlockTimeForwarder } from "../target/types/block_time_forwarder";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { ensureSettlementLookupTable, settlementLookupKeys } from "../client/lookupTable";
import { derivePaStatePda } from "../client/pda";
import { requirePubkey } from "./cli-utils";
import { getVerifierEntryPda, verifierOfEntry } from "../client/verifier";

async function main() {
  const provider = confirmedProvider();
  anchor.setProvider(provider);
  const wallet = provider.wallet as anchor.Wallet;
  const adapter = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const blockTimeForwarder = anchor.workspace.BlockTimeForwarder as Program<BlockTimeForwarder>;

  const [paState] = derivePaStatePda(adapter.programId);
  const state = await adapter.account.paStateAccount.fetch(paState);
  const proofSelector = Buffer.from(state.proofSelector);

  // The verifier program is whatever the router entry the deployment pinned points at.
  const [verifierEntry] = getVerifierEntryPda(proofSelector, state.verifierRouter);
  const entry = await provider.connection.getAccountInfo(verifierEntry);
  if (!entry) throw new Error(`verifier entry ${verifierEntry.toBase58()} does not exist on this cluster`);
  const verifierProgram = verifierOfEntry(entry.data);

  const existing = process.env.PA_LOOKUP_TABLE
    ? requirePubkey("PA_LOOKUP_TABLE", "the deployment's settlement lookup table, as a base58 pubkey")
    : undefined;

  const keys = settlementLookupKeys({
    paProgram: adapter.programId,
    verifierRouter: state.verifierRouter,
    proofSelector,
    verifierProgram,
    blockTimeForwarder: blockTimeForwarder.programId,
  });
  const { table, added, signature } = await ensureSettlementLookupTable(
    provider.connection,
    wallet.payer,
    keys,
    existing,
  );

  console.log(`settlement lookup table: ${table.key.toBase58()} (authority ${wallet.publicKey.toBase58()})`);
  for (const key of keys) {
    console.log(`  ${added.some((a) => a.equals(key)) ? "+" : "="} ${key.toBase58()}`);
  }
  console.log(signature ? `${added.length} key(s) added in ${signature}` : "0 keys added, nothing sent");
  if (!existing) {
    console.log(
      "Record the address in the cluster's deployment record and in anoma-pa-solana-client's SETTLE_LOOKUP_TABLE.",
    );
  }
}

main().catch((err) => {
  console.error("❌ lookup-table failed:", err.message || err);
  process.exit(1);
});
