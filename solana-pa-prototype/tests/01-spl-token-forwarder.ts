/**
 * SPL token forwarder: config initialization and the guards a direct caller
 * hits. Everything forward_call does past its caller check needs the
 * adapter as the CPI caller, so those behaviours are tested through
 * settlement in the adapter suite (tests/solana-pa-prototype.ts, the SPL
 * token forwarder block); the emergency flow is zz-forwarder-emergency.ts.
 *
 * Touches nothing of the adapter's state: the adapter suite's first test
 * needs an uninitialized adapter.
 */
import * as anchor from "@coral-xyz/anchor";
import { Program } from "@coral-xyz/anchor";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { ProtocolAdapter } from "../target/types/protocol_adapter";
import { SplTokenForwarder } from "../target/types/spl_token_forwarder";
import {
  EMERGENCY_COMMITTEE_LABEL,
  OP_UNWRAP,
  assertRejects,
  deriveConfigPda,
  derivePaStatePda,
  encodeUnwrapInput,
  initializeForwarder,
  makeFunder,
  requireFixture,
  seededKeypair,
} from "./utils";

describe("01-spl-token-forwarder (config and direct-call guards)", () => {
  const provider = anchor.AnchorProvider.env();
  anchor.setProvider(provider);

  const paProgram = anchor.workspace.ProtocolAdapter as Program<ProtocolAdapter>;
  const forwarderProgram = anchor.workspace.SplTokenForwarder as Program<SplTokenForwarder>;

  const [paState] = derivePaStatePda(paProgram.programId);
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const authority = Keypair.generate();
  const emergencyCommittee = seededKeypair(EMERGENCY_COMMITTEE_LABEL);

  // The logic ref the fixtures were proven under (the passthrough guest);
  // the config must authorize it for the adapter suite's wrap and unwrap.
  const logicRef = Array.from(
    Buffer.from(requireFixture("spl_token_wrap.json", "--spl-token-wrap").spl_token_wrap!.logic_ref_b64, "base64")
  );

  before(async () => {
    await funder.fund(authority, 2);
    await funder.fund(emergencyCommittee, 1);
  });

  const initialize = (adapter: PublicKey, ref: number[], committee: PublicKey) =>
    initializeForwarder(forwarderProgram, adapter, ref, committee, authority.publicKey).signers([authority]).rpc();

  describe("initialize", () => {
    // Mirrors ForwarderBase.t.sol and EmergencyMigratableForwarderBase.t.sol:
    // test_constructor_reverts_if_the_{protocol_adapter_address,logic_ref,emergency_committe_address}_is_zero
    for (const [name, adapter, ref, committee] of [
      ["protocol adapter address", PublicKey.default, logicRef, emergencyCommittee.publicKey],
      ["logic ref", paProgram.programId, Array(32).fill(0), emergencyCommittee.publicKey],
      ["emergency committee", paProgram.programId, logicRef, PublicKey.default],
    ] as const) {
      it(`rejects a zero ${name}`, () => assertRejects(initialize(adapter, ref, committee), /ZeroAddressNotAllowed/));
    }

    // Mirrors ForwarderBase.t.sol getProtocolAdapter/getLogicRef and
    // EmergencyMigratableForwarderBase.t.sol emergencyCaller-is-zero-before-set.
    it("stores the adapter, logic ref and committee, with no emergency caller", async () => {
      await initialize(paProgram.programId, logicRef, emergencyCommittee.publicKey);

      const config = await forwarderProgram.account.config.fetch(configPda);
      assert.ok(config.protocolAdapter.equals(paProgram.programId));
      assert.deepEqual(config.logicRef, logicRef);
      assert.ok(config.emergencyCommittee.equals(emergencyCommittee.publicKey));
      assert.ok(config.emergencyCaller.equals(PublicKey.default));
    });
  });

  describe("guards", () => {
    // Mirrors ForwarderBase.t.sol: test_forwardCall_reverts_if_the_pa_is_not_the_caller.
    // The forwarder reads the current top-level instruction's program id from
    // the instructions sysvar; a direct call sees itself, not the adapter.
    it("rejects forward_call that is not a CPI from the adapter", () => {
      const operand = encodeUnwrapInput(Keypair.generate().publicKey, 1000n, Keypair.generate().publicKey);
      return assertRejects(
        forwarderProgram.methods
          .forwardCall(logicRef, Buffer.concat([Buffer.from([OP_UNWRAP]), operand]))
          .accountsPartial({ config: configPda, ixSysvar: SYSVAR_INSTRUCTIONS_PUBKEY })
          .rpc(),
        /UnauthorizedCaller/
      );
    });

    // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
    it("rejects set_emergency_caller from a non-committee signer", async () => {
      const impostor = Keypair.generate();
      await funder.fund(impostor, 1);
      await assertRejects(
        forwarderProgram.methods
          .setEmergencyCaller(Keypair.generate().publicKey)
          .accounts({ committee: impostor.publicKey, paState })
          .signers([impostor])
          .rpc(),
        /UnauthorizedCaller/
      );
    });
  });

  after(() => funder.drainAll());
});
