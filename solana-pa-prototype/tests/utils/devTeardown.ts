/**
 * Builders for the adapter's `dev-teardown` instructions, which production
 * builds do not compile in. They live apart from helpers.ts because the
 * operator scripts that run against production builds import helpers.ts, and
 * naming one of these instructions there would stop them compiling against
 * production types. Only the suite and close-pdas.ts import this module.
 */
import { Program } from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { derivePaStatePda } from "./pda";

// A zero-argument entry of program.methods, typed loosely on purpose: naming
// a dev-teardown method in a type annotation would make this module fail to
// compile against production types, preempting the actionable error below.
type MethodBuilder = () => ReturnType<Program<ProtocolAdapter>["methods"][keyof Program<ProtocolAdapter>["methods"]]>;

/**
 * The builder of a feature-gated instruction, or an actionable error when
 * the program was built without its feature. The lookup goes through
 * program.methods, the camelCased surface Anchor's client exposes.
 */
function requireInstruction(program: Program<ProtocolAdapter>, instructionName: string): MethodBuilder {
  const camel = instructionName.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
  const method = (program.methods as Record<string, unknown>)[camel];
  if (typeof method !== "function") {
    throw new Error(
      `Instruction '${instructionName}' is not present in the program's IDL (target/idl/protocol_adapter.json): ` +
        `the program was built without the 'dev-teardown' feature, or the instruction does not exist on this branch.`
    );
  }
  return method as MethodBuilder;
}

/** `close_markers_batch` by `authority` over `markers`; only on a stopped adapter. */
export function closeMarkersBatch(program: Program<ProtocolAdapter>, authority: PublicKey, markers: PublicKey[]) {
  return requireInstruction(program, "close_markers_batch")()
    .accounts({ paState: derivePaStatePda(program.programId)[0], authority })
    .remainingAccounts(markers.map((pubkey) => ({ pubkey, isWritable: true, isSigner: false })));
}

/**
 * Close every marker account (the zero-byte nullifier and root markers) the
 * adapter owns, in batches, as `authority`: the provider wallet, which must
 * be the PA authority of a stopped adapter. Returns how many were closed.
 */
export async function closeAllMarkers(program: Program<ProtocolAdapter>, authority: PublicKey): Promise<number> {
  const markers = await program.provider.connection.getProgramAccounts(program.programId, { filters: [{ dataSize: 0 }] });
  const BATCH_SIZE = 20;
  for (let i = 0; i < markers.length; i += BATCH_SIZE) {
    await closeMarkersBatch(program, authority, markers.slice(i, i + BATCH_SIZE).map(({ pubkey }) => pubkey)).rpc();
  }
  return markers.length;
}
