/**
 * SPL token forwarder emergency flow on a paused adapter: the committee
 * names an emergency caller, who withdraws from escrow through
 * forward_emergency_call. The before hook initializes the adapter and the
 * forwarder config (no emergency caller), then stops the adapter.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { createAccount, getAccount } from "@solana/spl-token";
import { assert } from "chai";
import { emergencyWithdraw } from "../client/instructions";
import { localSetEmergencyCaller } from "./utils/localOnly";
import { deriveConfigPda } from "../client/pda";
import { approvedTokenAccount, createFundedEscrow, makeFunder, randomRef, assertFails } from "./utils/helpers";
import {
  ensureAdapterInitialized,
  forwarderProgram,
  initForwarderConfig,
  paState,
  provider,
  pauseAsOwner,
} from "./utils/adapterSuite";

describe("forwarder emergency (adapter paused)", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const authority = Keypair.generate();
  const emergencyCommittee = Keypair.generate();
  const emergencyCaller = Keypair.generate();
  const recipient = Keypair.generate();

  let mint: PublicKey;
  let escrowAuthority: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;

  before(async () => {
    await funder.fund(authority, 2);
    await funder.fund(recipient, 1);
    await funder.fund(emergencyCommittee, 1);
    await funder.fund(emergencyCaller, 1);

    await ensureAdapterInitialized();
    await initForwarderConfig(randomRef(), emergencyCommittee.publicKey);
    await pauseAsOwner();

    ({ mint, escrowAuthority, escrowAta } = await createFundedEscrow(
      provider,
      forwarderProgram.programId,
      authority,
      100_000_000n,
    ));
    recipientAta = await createAccount(provider.connection, recipient, mint, recipient.publicKey);
  });

  const withdraw = (
    caller: Keypair,
    amount: bigint,
    accounts: Partial<{ escrowAta: PublicKey; recipientAta: PublicKey }> = {},
  ) =>
    emergencyWithdraw(
      forwarderProgram,
      paState,
      caller.publicKey,
      { mint, amount, recipient: recipient.publicKey },
      { escrowAta, recipientAta, ...accounts },
    )
      .signers([caller])
      .rpc();

  const setEmergencyCallerAsCommittee = (caller: PublicKey) =>
    localSetEmergencyCaller(provider.connection, forwarderProgram, emergencyCommittee.publicKey, paState, caller)
      .signers([emergencyCommittee])
      .rpc();

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
  it("rejects forward_emergency_call before an emergency caller is set", async () => {
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(PublicKey.default), "no emergency caller yet");
    await assertFails(withdraw(emergencyCaller, 1000n), { program: forwarderProgram, error: "EmergencyCallerNotSet" });
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
  it("rejects a zero emergency caller", () =>
    assertFails(setEmergencyCallerAsCommittee(PublicKey.default), {
      program: forwarderProgram,
      error: "ZeroAddressNotAllowed",
    }));

  // Mirrors: test_setEmergencyCaller_sets_the_emergency_caller and
  // test_emergencyCaller_returns_the_emergency_caller_after_it_has_been_set
  it("lets the committee set the emergency caller once the adapter is paused", async () => {
    await setEmergencyCallerAsCommittee(emergencyCaller.publicKey);
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
  it("rejects setting the emergency caller twice", () =>
    assertFails(setEmergencyCallerAsCommittee(Keypair.generate().publicKey), {
      program: forwarderProgram,
      error: "EmergencyCallerAlreadySet",
    }));

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
  it("rejects forward_emergency_call from anyone but the emergency caller", async () => {
    const wrongCaller = await funder.fresh(1);
    await assertFails(withdraw(wrongCaller, 1000n), { program: forwarderProgram, error: "UnauthorizedCaller" });
  });

  // The destination is chosen by the caller, not by the input; it must be the recipient's.
  it("rejects a withdrawal to a token account the recipient does not own", async () => {
    const foreignAta = await createAccount(provider.connection, authority, mint, authority.publicKey);
    await assertFails(withdraw(emergencyCaller, 1000n, { recipientAta: foreignAta }), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });
  });

  // The escrow authority signs the withdrawal; it pays only from an account the escrow owns.
  it("rejects a withdrawal whose source the escrow does not own", async () => {
    const otherAta = await approvedTokenAccount(provider.connection, funder, mint, authority, escrowAuthority, 1000);
    await assertFails(withdraw(emergencyCaller, 1000n, { escrowAta: otherAta }), {
      program: forwarderProgram,
      error: "WrongTokenAccountOwner",
    });
    assert.equal((await getAccount(provider.connection, otherAta)).amount, 1000n, "no tokens move");
  });

  // Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
  it("lets the emergency caller withdraw from escrow", async () => {
    const amount = 25_000_000n;
    const balances = () =>
      Promise.all([getAccount(provider.connection, escrowAta), getAccount(provider.connection, recipientAta)]);
    const [escrowBefore, recipientBefore] = await balances();

    await withdraw(emergencyCaller, amount);

    const [escrowAfter, recipientAfter] = await balances();
    assert.equal(escrowAfter.amount, escrowBefore.amount - amount);
    assert.equal(recipientAfter.amount, recipientBefore.amount + amount);
  });

  after(() => funder.drainAll());
});
