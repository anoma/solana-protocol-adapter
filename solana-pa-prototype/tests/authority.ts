/**
 * The adapter authority: its record after initialize, and the two-step
 * transfer (propose, accept, cancel). Every test starts and ends with the
 * provider wallet as the authority.
 */
import { PublicKey, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { AUTHORITY_MISMATCH_PATTERN, errorHaystack, emergencyStop } from "./utils";
import {
  provider,
  program,
  paState,
  ensureAdapterInitialized,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (Issue #6: Emergency Stop)", () => {
  const { funder } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

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
    const nonAuthority = Keypair.generate();
    await funder.fund(nonAuthority, 1);

    try {
      await emergencyStop(program, nonAuthority.publicKey).signers([nonAuthority]).rpc();
      assert.fail("expected emergency_stop to fail for non-authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      // Anchor's has_one constraint produces "A has one constraint was violated"
      // or our custom error "Unauthorized"
      assert.match(haystack, AUTHORITY_MISMATCH_PATTERN, "Should fail with Unauthorized or has_one constraint error");
    }
  });

  it("rejects propose_authority from non-authority", async () => {
    const nonAuthority = Keypair.generate();
    const newAuthority = Keypair.generate();
    await funder.fund(nonAuthority, 1);

    try {
      await program.methods
        .proposeAuthority(newAuthority.publicKey)
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected propose_authority to fail for non-authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(haystack, AUTHORITY_MISMATCH_PATTERN, "Should fail with Unauthorized or has_one constraint error");
    }
  });

  it("two-step authority transfer: propose + accept", async () => {
    const stateBefore = await program.account.paStateAccount.fetch(paState);
    const currentAuthority = stateBefore.authority;

    const newAuthority = Keypair.generate();
    await funder.fund(newAuthority, 1);

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

    const newAuthority = Keypair.generate();
    await funder.fund(newAuthority, 1);

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

    try {
      await emergencyStop(program, provider.wallet.publicKey).rpc();
      assert.fail("expected emergency_stop to fail for old authority");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(haystack, AUTHORITY_MISMATCH_PATTERN, "Should fail with Unauthorized or has_one constraint error");
    }

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
    const realCandidate = Keypair.generate();
    await funder.fund(realCandidate, 1);

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
    const random = Keypair.generate();
    await funder.fund(random, 1);

    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: random.publicKey,
        })
        .signers([random])
        .rpc();
      assert.fail("accept_authority should fail with no pending proposal");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });

  it("wrong signer cannot accept a pending proposal", async () => {
    const intended = Keypair.generate();
    const attacker = Keypair.generate();
    await funder.fund(attacker, 1);

    // Propose the intended authority
    await program.methods
      .proposeAuthority(intended.publicKey)
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Attacker tries to accept
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: attacker.publicKey,
        })
        .signers([attacker])
        .rpc();
      assert.fail("attacker should not be able to accept someone else's proposal");
    } catch (e: any) {
      assertPAError(e, "Unauthorized");
    }

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
    const firstCandidate = Keypair.generate();
    const secondCandidate = Keypair.generate();
    await funder.fund(firstCandidate, 1);
    await funder.fund(secondCandidate, 1);

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
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: firstCandidate.publicKey,
        })
        .signers([firstCandidate])
        .rpc();
      assert.fail("first candidate should not be able to accept after overwrite");
    } catch (e: any) {
      assertPAError(e, "Unauthorized");
    }

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
    const candidate = Keypair.generate();
    await funder.fund(candidate, 1);

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
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc();
      assert.fail("second accept should fail");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }

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
    const candidate = Keypair.generate();
    await funder.fund(candidate, 1);

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
    try {
      await program.methods
        .acceptAuthority()
        .accountsPartial({
          paState,
          newAuthority: candidate.publicKey,
        })
        .signers([candidate])
        .rpc();
      assert.fail("accept should fail after cancel");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });

  it("cancel_authority_transfer fails when no proposal is pending", async () => {
    try {
      await program.methods
        .cancelAuthorityTransfer()
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("cancel should fail with no pending proposal");
    } catch (e: any) {
      assertPAError(e, "NoPendingAuthority");
    }
  });
});
