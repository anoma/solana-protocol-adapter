/**
 * SPL token forwarder emergency flow. Runs after the adapter suite, whose
 * final test stops the adapter for good: the committee names an emergency
 * caller, who withdraws from escrow through forward_emergency_call.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey } from "@solana/web3.js";
import {
  createAccount,
  createMint,
  getAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_CALLER_LABEL,
  EMERGENCY_COMMITTEE_LABEL,
  OP_EMERGENCY_WITHDRAW,
  deriveConfigPda,
  deriveEscrowPda,
  derivePaStatePda,
  drainKeypairs,
  encodeEmergencyWithdrawInput,
  fundKeypair,
  seededKeypair,
} from "./utils";

describe("zz-forwarder-emergency (adapter stopped)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const paProgram = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;

  const [paState] = derivePaStatePda(paProgram.programId);
  const [configPda] = deriveConfigPda(forwarderProgram.programId);

  const fundedKeypairs: Keypair[] = [];
  async function fund(kp: Keypair, sol: number) {
    await fundKeypair(provider, kp, sol);
    fundedKeypairs.push(kp);
  }

  const authority = Keypair.generate();
  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);
  const emergencyCaller = seededKeypair(EMERGENCY_CALLER_LABEL);
  const recipient = Keypair.generate();

  let tokenMint: PublicKey;
  let escrowPda: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;
  const ESCROW_FUNDING = 100_000_000n;

  before(async () => {
    await fund(authority, 2);
    await fund(recipient, 1);
    await fund(emergencyCommittee, 1);
    await fund(emergencyCaller, 1);

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

    tokenMint = await createMint(provider.connection, authority, authority.publicKey, null, 6);
    [escrowPda] = deriveEscrowPda(forwarderProgram.programId, tokenMint);
    escrowAta = (await getOrCreateAssociatedTokenAccount(provider.connection, authority, tokenMint, escrowPda, true)).address;
    recipientAta = await createAccount(provider.connection, recipient, tokenMint, recipient.publicKey);
    await mintTo(provider.connection, authority, tokenMint, escrowAta, authority, Number(ESCROW_FUNDING));
  });

  function withdrawInput(amount: bigint): Buffer {
    return encodeEmergencyWithdrawInput(OP_EMERGENCY_WITHDRAW, tokenMint, amount, recipient.publicKey);
  }

  function withdrawAccounts() {
    return [
      { pubkey: escrowAta, isSigner: false, isWritable: true },
      { pubkey: recipientAta, isSigner: false, isWritable: true },
      { pubkey: escrowPda, isSigner: false, isWritable: false },
      { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
    ];
  }

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
  it("rejects forward_emergency_call before an emergency caller is set", async () => {
    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(PublicKey.default), "no emergency caller yet");

    try {
      await forwarderProgram.methods
        .forwardEmergencyCall(withdrawInput(1000n))
        .accounts({ caller: emergencyCaller.publicKey, paState })
        .remainingAccounts(withdrawAccounts())
        .signers([emergencyCaller])
        .rpc();
      assert.fail("expected forward_emergency_call to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerNotSet");
    }
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
  it("rejects a zero emergency caller", async () => {
    try {
      await forwarderProgram.methods
        .setEmergencyCaller(PublicKey.default)
        .accounts({ committee: emergencyCommittee.publicKey, paState })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("expected set_emergency_caller to fail");
    } catch (e: any) {
      assert.include(e.toString(), "ZeroAddressNotAllowed");
    }
  });

  // Mirrors: test_setEmergencyCaller_sets_the_emergency_caller and
  // test_emergencyCaller_returns_the_emergency_caller_after_it_has_been_set
  it("lets the committee set the emergency caller once the adapter is stopped", async () => {
    await forwarderProgram.methods
      .setEmergencyCaller(emergencyCaller.publicKey)
      .accounts({ committee: emergencyCommittee.publicKey, paState })
      .signers([emergencyCommittee])
      .rpc();

    const config = await forwarderProgram.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
  it("rejects setting the emergency caller twice", async () => {
    try {
      await forwarderProgram.methods
        .setEmergencyCaller(Keypair.generate().publicKey)
        .accounts({ committee: emergencyCommittee.publicKey, paState })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("expected set_emergency_caller to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerAlreadySet");
    }
  });

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
  it("rejects forward_emergency_call from anyone but the emergency caller", async () => {
    const wrongCaller = Keypair.generate();
    await fund(wrongCaller, 1);

    try {
      await forwarderProgram.methods
        .forwardEmergencyCall(withdrawInput(1000n))
        .accounts({ caller: wrongCaller.publicKey, paState })
        .remainingAccounts(withdrawAccounts())
        .signers([wrongCaller])
        .rpc();
      assert.fail("expected forward_emergency_call to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  // Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
  it("lets the emergency caller withdraw from escrow", async () => {
    const amount = 25_000_000n;
    const escrowBefore = (await getAccount(provider.connection, escrowAta)).amount;
    const recipientBefore = (await getAccount(provider.connection, recipientAta)).amount;

    await forwarderProgram.methods
      .forwardEmergencyCall(withdrawInput(amount))
      .accounts({ caller: emergencyCaller.publicKey, paState })
      .remainingAccounts(withdrawAccounts())
      .signers([emergencyCaller])
      .rpc();

    const escrowAfter = (await getAccount(provider.connection, escrowAta)).amount;
    const recipientAfter = (await getAccount(provider.connection, recipientAta)).amount;
    assert.equal(escrowAfter, escrowBefore - amount);
    assert.equal(recipientAfter, recipientBefore + amount);
  });

  after(async () => {
    await drainKeypairs(provider, fundedKeypairs);
    fundedKeypairs.length = 0;
  });
});
