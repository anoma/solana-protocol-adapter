import * as anchor from "@coral-xyz/anchor";
import { PublicKey } from "@solana/web3.js";
import { Program } from "@coral-xyz/anchor";
import { createHash } from "crypto";
import { ProtocolAdapter } from "../../target/types/protocol_adapter";
import { EMPTY_TREE_ROOT_INITIAL } from "./constants";
import { deriveRootMarkerPda } from "./pda";

// Predict the produced-root marker PDA for a settlement whose resulting root
// is only known after commitments are appended on-chain.
//
// Mirrors what a production submitter must do: fetch the current tree state
// and replay the append locally over the transaction's created commitments
// (merkle.rs `append_to_tree`, including the expand-after-fill growth step).
// A wrong prediction cannot settle: the program's
// `require_keys_eq!(expected_pda, provided)` rejects it with RootPdaMismatch,
// so the on-chain check keeps this replica honest.
export const MAX_TREE_DEPTH = 32;

export function hashTwo(left: Buffer, right: Buffer): Buffer {
  return createHash("sha256").update(left).update(right).digest();
}

// ZEROS[0] = PADDING_LEAF; ZEROS[i] = hash(ZEROS[i-1], ZEROS[i-1]) — the
// zero-subtree hashes from merkle.rs, derived rather than copied.
export const ZEROS: Buffer[] = (() => {
  const zeros: Buffer[] = [EMPTY_TREE_ROOT_INITIAL];
  for (let i = 1; i < MAX_TREE_DEPTH; i++) {
    zeros.push(hashTwo(zeros[i - 1], zeros[i - 1]));
  }
  return zeros;
})();

export type TreeState = {
  nextIndex: anchor.BN;
  currentDepth: number;
  frontier: number[][];
  root: number[];
};

export function computeRootAfterAppend(state: TreeState, leaves: Buffer[]): Buffer {
  let nextIndex = BigInt(state.nextIndex.toString());
  let depth = state.currentDepth;
  const frontier: Buffer[] = state.frontier.map((f) => Buffer.from(f));
  let root: Buffer = Buffer.from(state.root);

  for (const leaf of leaves) {
    if (nextIndex >= 1n << BigInt(depth)) {
      throw new Error("tree over capacity in root prediction");
    }
    let index = nextIndex;
    nextIndex += 1n;

    let current: Buffer = leaf;
    for (let level = 0; level < depth; level++) {
      if ((index & 1n) === 0n) {
        frontier[level] = current;
        current = hashTwo(current, ZEROS[level]);
      } else {
        current = hashTwo(frontier[level], current);
      }
      index >>= 1n;
    }

    if (nextIndex === 1n << BigInt(depth) && depth < MAX_TREE_DEPTH) {
      frontier.push(current);
      current = hashTwo(current, ZEROS[depth]);
      depth += 1;
    }

    root = current;
  }
  return root;
}

/** The marker PDA of the root the adapter will hold after appending `createdCommitments`. */
export async function predictRootMarkerPda(
  program: Program<ProtocolAdapter>,
  paState: PublicKey,
  createdCommitments: Buffer[]
): Promise<PublicKey> {
  const state = await program.account.paStateAccount.fetch(paState);
  const root = computeRootAfterAppend(state, createdCommitments);
  return deriveRootMarkerPda(paState, root, program.programId);
}
