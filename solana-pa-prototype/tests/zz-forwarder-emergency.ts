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
  approvedTokenAccount,
  assertRejects,
  createFundedEscrow,
  deriveConfigPda,
  derivePaStatePda,
  emergencyWithdraw,
  makeFunder,
  seededKeypair,
  setEmergencyCaller,
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

  let mint: PublicKey;
  let escrowPda: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;

  before(async () => {
    await funder.fund(authority, 2);
    await funder.fund(recipient, 1);
    await funder.fund(emergencyCommittee, 1);
    await funder.fund(emergencyCaller, 1);

    const [state, config] = await Promise.all([
      paProgram.account.paStateAccount.fetch(paState),
      provider.connection.getAccountInfo(configPda),
    ]);
    assert.deepEqual(state.lifecycle, { stopped: {} }, "the adapter suite's final emergency-stop test must have run before this file");
    assert.ok(config, "01-spl-token-forwarder.ts must have initialized the forwarder config");

    ({ mint, escrowPda, escrowAta } = await createFundedEscrow(provider, forwarderProgram.programId, authority, 100_000_000n));
    recipientAta = await createAccount(provider.connection, recipient, mint, recipient.publicKey);
  });

  const withdraw = (
    caller: Keypair,
    amount: bigint,
    accounts: Partial<{ escrowAta: PublicKey; recipientAta: PublicKey }> = {}
  ) =>
    emergencyWithdraw(
      forwarderProgram,
      paState,
      caller.publicKey,
      { mint, amount, recipient: recipient.publicKey },
      { escrowAta, recipientAta, escrowPda, ...accounts }
    )
      .signers([caller])
      .rpc();

  const setEmergencyCallerAsCommittee = (caller: PublicKey) =>
    setEmergencyCaller(forwarderProgram, emergencyCommittee.publicKey, paState, caller)
      .signers([emergencyCommittee])
      .rpc();

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
  it("rejects forward_emergency_call before an emergency caller is set", async () => {
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(PublicKey.default), "no emergency caller yet");
    await assertRejects(withdraw(emergencyCaller, 1000n), /EmergencyCallerNotSet/);
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
  it("rejects a zero emergency caller", () => assertRejects(setEmergencyCallerAsCommittee(PublicKey.default), /ZeroAddressNotAllowed/));

  // Mirrors: test_setEmergencyCaller_sets_the_emergency_caller and
  // test_emergencyCaller_returns_the_emergency_caller_after_it_has_been_set
  it("lets the committee set the emergency caller once the adapter is stopped", async () => {
    await setEmergencyCallerAsCommittee(emergencyCaller.publicKey);
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
  it("rejects setting the emergency caller twice", () =>
    assertRejects(setEmergencyCallerAsCommittee(Keypair.generate().publicKey), /EmergencyCallerAlreadySet/));

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
  it("rejects forward_emergency_call from anyone but the emergency caller", async () => {
    const wrongCaller = Keypair.generate();
    await funder.fund(wrongCaller, 1);
    await assertRejects(withdraw(wrongCaller, 1000n), /UnauthorizedCaller/);
  });

  // The destination is chosen by the caller, not by the input; it must be the recipient's.
  it("rejects a withdrawal to a token account the recipient does not own", async () => {
    const foreignAta = await createAccount(provider.connection, authority, mint, authority.publicKey);
    await assertRejects(withdraw(emergencyCaller, 1000n, { recipientAta: foreignAta }), /WrongTokenAccountOwner/);
  });

  // The escrow PDA signs the withdrawal; it pays only from an account the escrow owns.
  it("rejects a withdrawal whose source the escrow does not own", async () => {
    const otherAta = await approvedTokenAccount(provider.connection, funder, mint, authority, escrowPda, 1000);
    await assertRejects(withdraw(emergencyCaller, 1000n, { escrowAta: otherAta }), /WrongTokenAccountOwner/);
    assert.equal((await getAccount(provider.connection, otherAta)).amount, 1000n, "no tokens move");
  });

  // Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
  it("lets the emergency caller withdraw from escrow", async () => {
    const amount = 25_000_000n;
    const balances = () => Promise.all([getAccount(provider.connection, escrowAta), getAccount(provider.connection, recipientAta)]);
    const [escrowBefore, recipientBefore] = await balances();

    await withdraw(emergencyCaller, amount);

    const [escrowAfter, recipientAfter] = await balances();
    assert.equal(escrowAfter.amount, escrowBefore.amount - amount);
    assert.equal(recipientAfter.amount, recipientBefore.amount + amount);
  });

  after(() => funder.drainAll());
});
