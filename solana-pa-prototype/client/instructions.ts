/**
 * Instruction builders for the protocol adapter: the one form of each
 * instruction the operator scripts and the test suite both send. Builders
 * return an Anchor method builder; callers add signers and send.
 */
import { Program } from "@anchor-lang/core";
import { ComputeBudgetProgram, PublicKey, SystemProgram } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { derivePaStatePda } from "./pda";

// Protocol adapter governance

/**
 * The adapter's `initialize`, signed and paid by `payer`, the program's
 * upgrade authority, which it hands to the program's upgrade authority PDA:
 * it makes `initialOwner` the owner, starts on the empty kind tree, and
 * refuses a verifier the router has paused, read from the router's verifier
 * entry for the build's selector, which the IDL's seeds resolve.
 */
export function initializeAdapter(program: Program<ProtocolAdapter>, payer: PublicKey, initialOwner: PublicKey) {
  return program.methods.initialize(initialOwner).accountsPartial({
    paState: derivePaStatePda(program.programId)[0],
    payer,
    systemProgram: SystemProgram.programId,
  });
}

/**
 * `migrate_state`, signed by the owner: brings the state account from the
 * previous schema version to this build's, once, right after the upgrade.
 */
export function migrateState(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.migrateState().accountsPartial({
    authority,
    paState: derivePaStatePda(program.programId)[0],
  });
}

// Every builder below takes the adapter's owner, stored in its state, as
// `authority`.

/**
 * The most compute units a transaction may use. `upgrade` runs under it,
 * since it hashes the whole buffer (sha256's cost grows with the length)
 * before the loader's own work.
 */
export const MAX_COMPUTE_UNIT_LIMIT = 1_400_000;

/**
 * `upgrade` by the owner: the program's code becomes `buffer`'s, a loader
 * buffer the owner wrote (its authority); the buffer's rent goes to `spill`.
 */
export function upgradeAdapter(
  program: Program<ProtocolAdapter>,
  authority: PublicKey,
  buffer: PublicKey,
  spill: PublicKey,
) {
  return program.methods
    .upgrade()
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority, buffer, spill })
    .preInstructions([ComputeBudgetProgram.setComputeUnitLimit({ units: MAX_COMPUTE_UNIT_LIMIT })]);
}

/** A logic ref and the denylist to add it to: the one for consumed resources, or the one for created resources. */
export type DeniedLogicRef = { logicRef: number[]; consumed: boolean };

/** `deny_logic_refs` by the owner, which pays for the entries. */
export function denyLogicRefs(program: Program<ProtocolAdapter>, authority: PublicKey, logicRefs: DeniedLogicRef[]) {
  return program.methods
    .denyLogicRefs(logicRefs)
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `pause` by the owner: settlement stops until `unpause`. */
export function pauseAdapter(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.pause().accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `unpause` by the owner: settlement resumes. */
export function unpauseAdapter(program: Program<ProtocolAdapter>, authority: PublicKey) {
  return program.methods.unpause().accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}

/** `set_kind_tree_root` by the owner. */
export function setKindTreeRoot(program: Program<ProtocolAdapter>, authority: PublicKey, root: number[]) {
  return program.methods
    .setKindTreeRoot(root)
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}
