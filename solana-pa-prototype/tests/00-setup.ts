/**
 * Test suite setup - runs first (alphabetically) to initialize shared state.
 *
 * Initializes the Protocol Adapter state account required by all test suites.
 * This ensures PA is available for emergency operations tests in the forwarder
 * suite and other tests that depend on PA being initialized.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { PublicKey, SystemProgram } from "@solana/web3.js";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";
import {
  derivePaStatePda,
  deriveRootMarkerPda,
  parseSelectorFromFixture,
} from "./utils";
import { EMPTY_TREE_ROOT_INITIAL } from "./utils";
import { readJson, Fixture } from "./utils";
import { VERIFIER_ROUTER_ID } from "../scripts/verifier-utils";
import { existsSync } from "fs";
import path from "path";

describe("00-setup", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  let paProgram: Program<SolanaPaPrototype>;
  let paStatePda: PublicKey;

  before(async () => {
    try {
      paProgram = anchor.workspace
        .SolanaPaPrototype as Program<SolanaPaPrototype>;
    } catch (e) {
      console.log("PA program not found in workspace, skipping setup");
      return;
    }

    [paStatePda] = derivePaStatePda(paProgram.programId);
  });

  it("initializes Protocol Adapter state", async () => {
    if (!paProgram) return;

    // Check if already initialized
    try {
      await paProgram.account.paStateAccount.fetch(paStatePda);
      console.log("PA state already initialized");
      return;
    } catch {
      // Not initialized, proceed with initialization
    }

    const genesisRootMarkerPda = deriveRootMarkerPda(
      paStatePda,
      EMPTY_TREE_ROOT_INITIAL,
      paProgram.programId
    );

    const fixturePath = path.resolve(
      process.cwd(),
      "tests",
      "fixtures",
      "batch_groth16.json"
    );
    let proofSelector: number[];
    if (existsSync(fixturePath)) {
      const fixture = readJson<Fixture>(fixturePath);
      proofSelector = Array.from(parseSelectorFromFixture(fixture.selector));
    } else {
      proofSelector = [0x73, 0xc4, 0x57, 0xba];
    }

    await paProgram.methods
      .initialize(VERIFIER_ROUTER_ID, proofSelector)
      .accounts({
        paState: paStatePda,
        payer: provider.wallet.publicKey,
        systemProgram: SystemProgram.programId,
      })
      .remainingAccounts([
        { pubkey: genesisRootMarkerPda, isWritable: true, isSigner: false },
      ])
      .rpc();

    // Verify initialization
    const state = await paProgram.account.paStateAccount.fetch(paStatePda);
    console.log(
      `PA state initialized: authority=${state.authority.toBase58()}, lifecycle=${JSON.stringify(
        state.lifecycle
      )}`
    );
  });
});
