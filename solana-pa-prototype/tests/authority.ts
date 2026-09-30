/**
 * The adapter authority: its record after initialize, and the two-step
 * transfer (propose, accept, cancel). Every test starts and ends with the
 * provider wallet as the authority.
 */
import { PublicKey, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { emergencyStop } from "../client/instructions";
import { assertFails } from "./utils/helpers";
import { provider, program, paState, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (Issue #6: Emergency Stop)", () => {
  const { funder } = useAdapterSuite();

  it("stores authority on PAStateAccount after initialize", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority, "State should have authority field");
    const authorityBytes = state.authority.toBytes();
    const isZero = authorityBytes.every((b: number) => b === 0);
    assert.ok(!isZero, "Authority should not be all zeros");
  });

  it("initializes with paused=false", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      JSON.stringify(state.lifecycle),
      JSON.stringify({ running: {} }),
      "State should be Running after initialize",
    );
  });

  it("rejects emergency_stop from non-authority", async () => {
    const nonAuthority = await funder.fresh(1);

    await assertFails(emergencyStop(program, nonAuthority.publicKey).signers([nonAuthority]).rpc(), {
      program,
      error: "Unauthorized",
    });
  });

  it("rejects propose_authority from non-authority", async () => {
    const nonAuthority = await funder.fresh(1);
    const newAuthority = Keypair.generate();

    await assertFails(
      program.methods
        .proposeAuthority(newAuthority.publicKey)
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc(),
      { program, error: "Unauthorized" },
    );
  });

  it("two-step authority transfer: propose + accept", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const currentAuthority = stateBefore.authority;

    const newAuthority = await funder.fresh(1);

    // Step 1: propose
    await program.methods
      .proposeAuthority(newAuthority.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Authority hasn't changed yet
    const stateAfterPropose = await program.account.paStateAccount.fetch(paState);
    assert.ok(stateAfterPropose.authority.equals(currentAuthority), "Authority should NOT change after propose");

    // Step 2: accept (signed by new authority)
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    const stateAfterAccept = await program.account.paStateAccount.fetch(paState);
    assert.ok(stateAfterAccept.authority.equals(newAuthority.publicKey), "Authority should be updated after accept");

    // Restore: propose back, accept with provider wallet
    await program.methods
      .proposeAuthority(currentAuthority)
      .accountsPartial({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();

    const stateRestored = await program.account.paStateAccount.fetch(paState);
    assert.ok(stateRestored.authority.equals(currentAuthority), "Authority should be restored to original");
  });

  it("old authority cannot call emergency_stop after transfer", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const originalAuthority = stateBefore.authority;

    const newAuthority = await funder.fresh(1);

    // Two-step transfer
    await program.methods
      .proposeAuthority(newAuthority.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();

    await assertFails(emergencyStop(program, provider.wallet.publicKey).rpc(), { program, error: "Unauthorized" });

    // Restore
    await program.methods
      .proposeAuthority(originalAuthority)
      .accountsPartial({
        paState,
        authority: newAuthority.publicKey,
      })
      .signers([newAuthority])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("proposing zero address does not brick governance", async () => {
    // Propose transfer to zero address
    await program.methods
      .proposeAuthority(PublicKey.default)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Authority is still the provider — proposal doesn't transfer
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority.equals(provider.wallet.publicKey), "Authority should still be provider after propose");

    // Overwrite with a real candidate, complete transfer, then restore
    const realCandidate = await funder.fresh(1);

    await program.methods
      .proposeAuthority(realCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: realCandidate.publicKey,
      })
      .signers([realCandidate])
      .rpc();

    const stateAfter = await program.account.paStateAccount.fetch(paState);
    assert.ok(stateAfter.authority.equals(realCandidate.publicKey), "Authority should transfer to the real candidate");

    // Restore
    await program.methods
      .proposeAuthority(provider.wallet.publicKey)
      .accountsPartial({
        paState,
        authority: realCandidate.publicKey,
      })
      .signers([realCandidate])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("accept_authority fails without a pending proposal", async () => {
    const random = await funder.fresh(1);

    await assertFails(
      program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: random.publicKey,
        })
        .signers([random])
        .rpc(),
      { program, error: "NoPendingAuthority" },
    );
  });

  it("wrong signer cannot accept a pending proposal", async () => {
    const intended = Keypair.generate();
    const attacker = await funder.fresh(1);

    // Propose the intended authority
    await program.methods
      .proposeAuthority(intended.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Attacker tries to accept
    await assertFails(
      program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: attacker.publicKey,
        })
        .signers([attacker])
        .rpc(),
      { program, error: "Unauthorized" },
    );

    // Cancel the proposal
    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("overwrite invalidates previous proposal", async () => {
    const firstCandidate = await funder.fresh(1);
    const secondCandidate = await funder.fresh(1);

    // Propose first candidate
    await program.methods
      .proposeAuthority(firstCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Overwrite with second candidate
    await program.methods
      .proposeAuthority(secondCandidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // First candidate cannot accept
    await assertFails(
      program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: firstCandidate.publicKey,
        })
        .signers([firstCandidate])
        .rpc(),
      { program, error: "Unauthorized" },
    );

    // Cancel
    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("pending_authority is cleared after accept", async () => {
    const candidate = await funder.fresh(1);

    await program.methods
      .proposeAuthority(candidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: candidate.publicKey,
      })
      .signers([candidate])
      .rpc();

    // Second accept should fail — pending is cleared
    await assertFails(
      program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc(),
      { program, error: "NoPendingAuthority" },
    );

    // Restore authority
    await program.methods
      .proposeAuthority(provider.wallet.publicKey)
      .accountsPartial({
        paState,
        authority: candidate.publicKey,
      })
      .signers([candidate])
      .rpc();
    await program.methods
      .acceptAuthority()
      .accountsPartial({
        paState,
        newAuthority: provider.wallet.publicKey,
      })
      .rpc();
  });

  it("cancel_authority_transfer clears pending proposal", async () => {
    const candidate = await funder.fresh(1);

    await program.methods
      .proposeAuthority(candidate.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    await program.methods
      .cancelAuthorityTransfer()
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Accept should fail — cancelled
    await assertFails(
      program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc(),
      { program, error: "NoPendingAuthority" },
    );
  });

  it("cancel_authority_transfer fails when no proposal is pending", async () => {
    await assertFails(
      program.methods
        .cancelAuthorityTransfer()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc(),
      { program, error: "NoPendingAuthority" },
    );
  });
});
