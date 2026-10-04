/**
 * Instruction builders for the protocol adapter: the one form of each
 * instruction the operator scripts and the test suite both send. Builders
 * return an Anchor method builder; callers add signers and send.
 */
import { Program } from "@anchor-lang/core";
import { ComputeBudgetProgram, PublicKey, SystemProgram } from "@solana/web3.js";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { getVerifierEntryPda } from "./verifier";
import { derivePaStatePda } from "./pda";

// Protocol adapter governance

/**
 * The adapter's `initialize`, signed and paid by `payer`, the program's
 * upgrade authority, which it hands to the program's upgrade authority PDA:
 * it makes `initialOwner` the owner, pins the verifier, starts on the empty
 * kind table, and refuses a verifier the router has paused, read from the
 * router's verifier entry for `proofSelector`.
 */
export function initializeAdapter(
  program: Program<ProtocolAdapter>,
  payer: PublicKey,
  initialOwner: PublicKey,
  verifierRouter: PublicKey,
  proofSelector: number[],
) {
  return program.methods.initialize(initialOwner, verifierRouter, proofSelector).accountsPartial({
    paState: derivePaStatePda(program.programId)[0],
    verifierEntry: getVerifierEntryPda(Buffer.from(proofSelector), verifierRouter)[0],
    payer,
    systemProgram: SystemProgram.programId,
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

/** `deny_logic_ref` by the owner, which pays for the entry. */
export function denyLogicRef(program: Program<ProtocolAdapter>, authority: PublicKey, logicRef: number[]) {
  return program.methods
    .denyLogicRef(logicRef)
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

/** `set_kind_table_commitment` by the owner. */
export function setKindTableCommitment(program: Program<ProtocolAdapter>, authority: PublicKey, commitment: number[]) {
  return program.methods
    .setKindTableCommitment(commitment)
    .accountsPartial({ paState: derivePaStatePda(program.programId)[0], authority });
}
