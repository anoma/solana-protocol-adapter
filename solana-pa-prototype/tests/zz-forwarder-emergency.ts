/**
 * SPL token forwarder emergency flow. Runs after the adapter suite, whose
 * final test stops the adapter for good: the committee names an emergency
 * caller, who withdraws from escrow through forward_emergency_call.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import { createAccount, getAccount } from "@solana/spl-token";
import { assert } from "chai";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_CALLER_LABEL,
  EMERGENCY_COMMITTEE_LABEL,
  createFundedEscrow,
  deriveConfigPda,
  derivePaStatePda,
  emergencyWithdrawAccounts,
  encodeUnwrapInput,
  makeFunder,
  seededKeypair,
} from "./utils";

describe("zz-forwarder-emergency (adapter stopped)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const paProgram = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;

  const [paState] = derivePaStatePda(paProgram.programId);
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const authority = Keypair.generate();
  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);
  const emergencyCaller = seededKeypair(EMERGENCY_CALLER_LABEL);
  const recipient = Keypair.generate();

  let tokenMint: PublicKey;
  let escrowPda: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;

  before(async () => {
    await funder.fund(authority, 2);
    await funder.fund(recipient, 1);
    await funder.fund(emergencyCommittee, 1);
    await funder.fund(emergencyCaller, 1);

    const state = await paProgram.account.paStateAccount.fetch(paState);
    assert.deepEqual(
      state.lifecycle,
      { stopped: {} },
      "the adapter suite's final emergency-stop test must have run before this file"
    );
    assert.ok(
      await provider.connection.getAccountInfo(configPda),
      "01-spl-token-forwarder.ts must have initialized the forwarder config"
    );

    ({ mint: tokenMint, escrowPda, escrowAta } = await createFundedEscrow(
      provider,
      forwarderProgram.programId,
      authority,
      100_000_000n
    ));
    recipientAta = await createAccount(provider.connection, recipient, tokenMint, recipient.publicKey);
  });

  function withdraw(caller: Keypair, amount: bigint) {
    return forwarderProgram.methods
      .forwardEmergencyCall(encodeUnwrapInput(tokenMint, amount, recipient.publicKey, false))
      .accounts({ caller: caller.publicKey, paState })
      .remainingAccounts(emergencyWithdrawAccounts(escrowAta, recipientAta, escrowPda))
      .signers([caller])
      .rpc();
  }

  function setEmergencyCaller(caller: PublicKey) {
    return forwarderProgram.methods
      .setEmergencyCaller(caller)
      .accounts({ committee: emergencyCommittee.publicKey, paState })
      .signers([emergencyCommittee])
      .rpc();
  }

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
  it("rejects forward_emergency_call before an emergency caller is set", async () => {
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(PublicKey.default), "no emergency caller yet");

    try {
      await withdraw(emergencyCaller, 1000n);
      assert.fail("expected forward_emergency_call to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerNotSet");
    }
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
  it("rejects a zero emergency caller", async () => {
    try {
      await setEmergencyCaller(PublicKey.default);
      assert.fail("expected set_emergency_caller to fail");
    } catch (e: any) {
      assert.include(e.toString(), "ZeroAddressNotAllowed");
    }
  });

  // Mirrors: test_setEmergencyCaller_sets_the_emergency_caller and
  // test_emergencyCaller_returns_the_emergency_caller_after_it_has_been_set
  it("lets the committee set the emergency caller once the adapter is stopped", async () => {
    await setEmergencyCaller(emergencyCaller.publicKey);
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
  it("rejects setting the emergency caller twice", async () => {
    try {
      await setEmergencyCaller(Keypair.generate().publicKey);
      assert.fail("expected set_emergency_caller to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerAlreadySet");
    }
  });

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
  it("rejects forward_emergency_call from anyone but the emergency caller", async () => {
    const wrongCaller = Keypair.generate();
    await funder.fund(wrongCaller, 1);

    try {
      await withdraw(wrongCaller, 1000n);
      assert.fail("expected forward_emergency_call to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  // Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
  it("lets the emergency caller withdraw from escrow", async () => {
    const amount = 25_000_000n;
    const [escrowBefore, recipientBefore] = await Promise.all([
      getAccount(provider.connection, escrowAta),
      getAccount(provider.connection, recipientAta),
    ]);

    await withdraw(emergencyCaller, amount);

    const [escrowAfter, recipientAfter] = await Promise.all([
      getAccount(provider.connection, escrowAta),
      getAccount(provider.connection, recipientAta),
    ]);
    assert.equal(escrowAfter.amount, escrowBefore.amount - amount);
    assert.equal(recipientAfter.amount, recipientBefore.amount + amount);
  });

  after(() => funder.drainAll());
});
