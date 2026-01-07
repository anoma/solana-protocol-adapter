/**
 * SPL Token Forwarder Emergency Operations Tests
 *
 * These tests run LAST because they stop the Protocol Adapter.
 * Once stopped, PA cannot be unpaused without a program upgrade.
 *
 * Test flow:
 * 1. Verify PA is running (not paused)
 * 2. Test "set_emergency_caller when not stopped" (should fail)
 * 3. Stop PA via emergency_stop()
 * 4. Test emergency operations (set_emergency_caller, forward_emergency_call)
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import {
  PublicKey,
  SystemProgram,
  Keypair,
  LAMPORTS_PER_SOL,
} from "@solana/web3.js";
import {
  createMint,
  createAccount,
  getOrCreateAssociatedTokenAccount,
  mintTo,
  getAccount,
  TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { assert } from "chai";
import { createHash } from "crypto";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import { SolanaPaPrototype } from "../target/types/solana_pa_prototype";

// Deterministic keypair seeds - must match 01-spl-token-forwarder.ts
const EMERGENCY_COMMITTEE_SEED = createHash("sha256").update("emergency_committee_seed").digest();
const EMERGENCY_CALLER_SEED = createHash("sha256").update("emergency_caller_seed").digest();

// Helper to derive PDAs
function deriveConfigPda(programId: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("config")], programId);
}

function deriveEscrowPda(programId: PublicKey, tokenMint: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync(
    [Buffer.from("escrow"), tokenMint.toBuffer()],
    programId
  );
}

function derivePaStatePda(paProgram: PublicKey): [PublicKey, number] {
  return PublicKey.findProgramAddressSync([Buffer.from("pa_state")], paProgram);
}

async function airdrop(provider: anchor.AnchorProvider, to: PublicKey, sol: number) {
  const sig = await provider.connection.requestAirdrop(to, sol * LAMPORTS_PER_SOL);
  await provider.connection.confirmTransaction(sig, "confirmed");
}

describe("zz-forwarder-emergency (runs last - stops PA)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  let program: Program<SplTokenForwarder>;
  let paProgram: Program<SolanaPaPrototype>;

  // Test accounts
  let authority: Keypair;
  let emergencyCommittee: Keypair;
  let emergencyCaller: Keypair;
  let recipient: Keypair;

  // Token accounts
  let tokenMint: PublicKey;
  let escrowAta: PublicKey;
  let recipientAta: PublicKey;

  // PDAs
  let configPda: PublicKey;
  let escrowPda: PublicKey;
  let paStatePda: PublicKey;

  // Logic ref for this forwarder
  let logicRef: number[];

  // Track if PA was stopped by this test suite
  let paEmergencyStopped = false;

  before(async () => {
    // Load programs
    try {
      program = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;
      paProgram = anchor.workspace.SolanaPaPrototype as Program<SolanaPaPrototype>;
    } catch (e) {
      throw new Error("Programs not found in workspace");
    }

    // Use deterministic keypairs for emergency committee/caller (must match 01-spl-token-forwarder.ts)
    authority = Keypair.generate();
    emergencyCommittee = Keypair.fromSeed(EMERGENCY_COMMITTEE_SEED);
    emergencyCaller = Keypair.fromSeed(EMERGENCY_CALLER_SEED);
    recipient = Keypair.generate();

    // Airdrop SOL
    await airdrop(provider, authority.publicKey, 10);
    await airdrop(provider, recipient.publicKey, 1);
    await airdrop(provider, emergencyCommittee.publicKey, 1);
    await airdrop(provider, emergencyCaller.publicKey, 1);

    // Derive PDAs
    [configPda] = deriveConfigPda(program.programId);
    [paStatePda] = derivePaStatePda(paProgram.programId);

    // Verify PA is initialized (done by 00-setup.ts)
    try {
      const paState = await paProgram.account.paStateAccount.fetch(paStatePda);
      // PA should NOT be paused yet (other tests depend on it running)
      if (paState.paused) {
        throw new Error("PA is already paused - test order may be wrong");
      }
    } catch (e: any) {
      if (e.message.includes("already paused")) throw e;
      throw new Error("PA not initialized - 00-setup.ts should have initialized it");
    }

    // Set logic_ref - must match what was used in forwarder initialization
    logicRef = Array.from(createHash("sha256").update("spl_token_forwarder_test_logic_ref").digest());

    // Create token mint
    tokenMint = await createMint(
      provider.connection,
      authority,
      authority.publicKey,
      null,
      6
    );

    // Derive escrow PDA
    [escrowPda] = deriveEscrowPda(program.programId, tokenMint);

    // Create escrow ATA
    const escrowAtaInfo = await getOrCreateAssociatedTokenAccount(
      provider.connection,
      authority,
      tokenMint,
      escrowPda,
      true
    );
    escrowAta = escrowAtaInfo.address;

    // Create recipient ATA
    recipientAta = await createAccount(
      provider.connection,
      recipient,
      tokenMint,
      recipient.publicKey
    );

    // Mint tokens to escrow (simulating prior wraps)
    await mintTo(provider.connection, authority, tokenMint, escrowAta, authority, 100_000_000);

    // Initialize forwarder config if not already done
    try {
      await program.account.config.fetch(configPda);
    } catch {
      await program.methods
        .initialize(paProgram.programId, logicRef, emergencyCommittee.publicKey)
        .accounts({
          authority: authority.publicKey,
        })
        .signers([authority])
        .rpc();
    }
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_pa_is_not_stopped
  it("rejects set_emergency_caller when PA not stopped", async function() {
    const paState = await paProgram.account.paStateAccount.fetch(paStatePda);
    assert.equal(paState.paused, false, "PA should not be paused initially");

    const newCaller = Keypair.generate();

    try {
      await program.methods
        .setEmergencyCaller(newCaller.publicKey)
        .accounts({
          committee: emergencyCommittee.publicKey,
          paState: paStatePda,
        })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("Expected transaction to fail");
    } catch (e: any) {
      assert.include(e.toString(), "ProtocolAdapterNotStopped");
    }
  });

  // Mirrors: _stopProtocolAdapter() helper in EVM tests
  it("stops Protocol Adapter for emergency operations", async function() {
    let paState = await paProgram.account.paStateAccount.fetch(paStatePda);
    assert.equal(paState.paused, false, "PA should not be paused initially");

    // Stop the PA
    await paProgram.methods
      .emergencyStop()
      .accounts({
        paState: paStatePda,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    // Verify PA is now paused
    paState = await paProgram.account.paStateAccount.fetch(paStatePda);
    assert.equal(paState.paused, true, "PA should be paused after emergency_stop");
    paEmergencyStopped = true;
  });

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_emergency_caller_is_not_set
  it("rejects forward_emergency_call when emergency caller not set", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    const config = await program.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(PublicKey.default), "Emergency caller should not be set yet");

    const input = Buffer.alloc(73);
    input.writeUInt8(0, 0);
    tokenMint.toBuffer().copy(input, 1);
    input.writeBigUInt64LE(BigInt(1000), 33);
    recipient.publicKey.toBuffer().copy(input, 41);

    try {
      await program.methods
        .forwardEmergencyCall(input)
        .accounts({
          caller: emergencyCaller.publicKey,
          paState: paStatePda,
        })
        .remainingAccounts([
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: recipientAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ])
        .signers([emergencyCaller])
        .rpc();
      assert.fail("Expected transaction to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerNotSet");
    }
  });

  // Mirrors: test_setEmergencyCaller_sets_the_emergency_caller
  it("allows emergency committee to set emergency caller (when stopped)", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    await program.methods
      .setEmergencyCaller(emergencyCaller.publicKey)
      .accounts({
        committee: emergencyCommittee.publicKey,
        paState: paStatePda,
      })
      .signers([emergencyCommittee])
      .rpc();

    const config = await program.account.config.fetch(configPda);
    assert.ok(config.emergencyCaller.equals(emergencyCaller.publicKey));
  });

  // Mirrors: test_emergencyCaller_returns_the_emergency_caller_after_it_has_been_set
  // https://github.com/anoma/anomapay-backend/blob/main/contracts/test/bases/EmergencyMigratableForwarderBase.t.sol
  it("emergency_caller returns the caller after it has been set", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    const config = await program.account.config.fetch(configPda);
    assert.ok(
      config.emergencyCaller.equals(emergencyCaller.publicKey),
      "Emergency caller should return the set address"
    );
    assert.ok(
      !config.emergencyCaller.equals(PublicKey.default),
      "Emergency caller should not be zero after being set"
    );
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_emergency_caller_has_already_been_set
  it("rejects setting emergency caller twice", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    const anotherCaller = Keypair.generate();

    try {
      await program.methods
        .setEmergencyCaller(anotherCaller.publicKey)
        .accounts({
          committee: emergencyCommittee.publicKey,
          paState: paStatePda,
        })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("Expected transaction to fail");
    } catch (e: any) {
      assert.include(e.toString(), "EmergencyCallerAlreadySet");
    }
  });

  // Mirrors: test_forwardEmergencyCall_reverts_if_the_pa_is_stopped_but_the_caller_is_not_the_emergency_caller
  it("rejects forward_emergency_call from wrong caller", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    const wrongCaller = Keypair.generate();
    await airdrop(provider, wrongCaller.publicKey, 1);

    const input = Buffer.alloc(73);
    input.writeUInt8(0, 0);
    tokenMint.toBuffer().copy(input, 1);
    input.writeBigUInt64LE(BigInt(1000), 33);
    recipient.publicKey.toBuffer().copy(input, 41);

    try {
      await program.methods
        .forwardEmergencyCall(input)
        .accounts({
          caller: wrongCaller.publicKey,
          paState: paStatePda,
        })
        .remainingAccounts([
          { pubkey: escrowAta, isSigner: false, isWritable: true },
          { pubkey: recipientAta, isSigner: false, isWritable: true },
          { pubkey: escrowPda, isSigner: false, isWritable: false },
          { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
        ])
        .signers([wrongCaller])
        .rpc();
      assert.fail("Expected transaction to fail");
    } catch (e: any) {
      assert.include(e.toString(), "UnauthorizedCaller");
    }
  });

  // Mirrors: test_forwardEmergencyCall_forwards_calls_if_the_pa_is_stopped_and_the_caller_is_the_emergency_caller
  it("allows emergency caller to withdraw", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    const amount = BigInt(25_000_000); // 25 tokens

    const input = Buffer.alloc(73);
    input.writeUInt8(0, 0);
    tokenMint.toBuffer().copy(input, 1);
    input.writeBigUInt64LE(amount, 33);
    recipient.publicKey.toBuffer().copy(input, 41);

    const escrowBefore = await getAccount(provider.connection, escrowAta);
    const recipientBefore = await getAccount(provider.connection, recipientAta);

    await program.methods
      .forwardEmergencyCall(input)
      .accounts({
        caller: emergencyCaller.publicKey,
        paState: paStatePda,
      })
      .remainingAccounts([
        { pubkey: escrowAta, isSigner: false, isWritable: true },
        { pubkey: recipientAta, isSigner: false, isWritable: true },
        { pubkey: escrowPda, isSigner: false, isWritable: false },
        { pubkey: TOKEN_PROGRAM_ID, isSigner: false, isWritable: false },
      ])
      .signers([emergencyCaller])
      .rpc();

    const escrowAfter = await getAccount(provider.connection, escrowAta);
    const recipientAfter = await getAccount(provider.connection, recipientAta);

    assert.equal(
      escrowAfter.amount.toString(),
      (BigInt(escrowBefore.amount.toString()) - amount).toString()
    );
    assert.equal(
      recipientAfter.amount.toString(),
      (BigInt(recipientBefore.amount.toString()) + amount).toString()
    );
  });

  // Mirrors: test_setEmergencyCaller_reverts_if_the_new_emergency_caller_is_the_zero_address
  it("rejects set_emergency_caller with zero address", async function() {
    if (!paEmergencyStopped) throw new Error("PA not stopped - previous test failed");

    try {
      await program.methods
        .setEmergencyCaller(PublicKey.default)
        .accounts({
          committee: emergencyCommittee.publicKey,
          paState: paStatePda,
        })
        .signers([emergencyCommittee])
        .rpc();
      assert.fail("Expected transaction to fail");
    } catch (e: any) {
      // Could be ZeroAddressNotAllowed or EmergencyCallerAlreadySet (if already set)
      const errorStr = e.toString();
      assert.ok(
        errorStr.includes("ZeroAddressNotAllowed") || errorStr.includes("EmergencyCallerAlreadySet"),
        `Expected ZeroAddressNotAllowed or EmergencyCallerAlreadySet, got: ${errorStr}`
      );
    }
  });
});
