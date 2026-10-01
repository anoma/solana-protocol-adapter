/**
 * Builders for the adapter's `dev-teardown` instructions, which production
 * builds do not compile in. They are kept out of instructions.ts so that
 * only the tools meant for dev deployments reach them: the suite and
 * close-pdas.ts import this module, and the operator scripts that run
 * against production deployments do not.
 */
import { Program } from "@anchor-lang/core";
import { PublicKey } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { chunks } from "./instructions";
import { derivePaStatePda } from "./pda";

/**
 * `close_markers_batch` by `authority` over `markers`; only on a stopped
 * adapter. The method is looked up untyped: naming it in a type would make
 * this module fail to compile against production types, preempting the
 * actionable error thrown when the program was built without the feature.
 */
export function closeMarkersBatch(program: Program<ProtocolAdapter>, authority: PublicKey, markers: PublicKey[]) {
  const method = (program.methods as Record<string, unknown>).closeMarkersBatch;
  if (typeof method !== "function") {
    throw new Error(
      "Instruction 'close_markers_batch' is not present in the program's IDL (target/idl/protocol_adapter.json): " +
        "the program was built without the 'dev-teardown' feature.",
    );
  }
  return (method as () => ReturnType<Program<ProtocolAdapter>["methods"][keyof Program<ProtocolAdapter>["methods"]]>)()
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority })
    .remainingAccounts(markers.map((pubkey) => ({ pubkey, isWritable: true, isSigner: false })));
}

/**
 * Close every marker account (the zero-byte nullifier and root markers) the
 * adapter owns, in batches, as `authority`: the provider wallet, which must
 * be the program's upgrade authority, on a stopped adapter. Returns how many
 * were closed.
 */
export async function closeAllMarkers(program: Program<ProtocolAdapter>, authority: PublicKey): Promise<number> {
  const markers = await program.provider.connection.getProgramAccounts(program.programId, {
    filters: [{ dataSize: 0 }],
  });
  const BATCH_SIZE = 20;
  for (const batch of chunks(markers, BATCH_SIZE)) {
    await closeMarkersBatch(
      program,
      authority,
      batch.map(({ pubkey }) => pubkey),
    ).rpc();
  }
  return markers.length;
}
