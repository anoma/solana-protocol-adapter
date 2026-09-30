/**
 * SPL token forwarder config: logic-ref rotation and the guards a direct
 * caller hits. The before hook initializes the config. Everything
 * forward_call does past its caller check needs the adapter as the CPI
 * caller, so those behaviours are tested through settlement
 * (spl-token-wrap-unwrap.ts); the emergency flow is forwarder-emergency.ts.
 */
import * as anchor from "@anchor-lang/core";
import { Keypair, PublicKey, SYSVAR_INSTRUCTIONS_PUBKEY } from "@solana/web3.js";
import { assert } from "chai";
import { encodeUnwrapInput, setLogicRef, setEmergencyCaller } from "../client/instructions";
import { OP_UNWRAP } from "../client/constants";
import { deriveConfigPda, deriveProgramDataPda } from "../client/pda";
import { confirmedTransaction, makeFunder, randomRef, assertFails } from "./utils/helpers";
import { forwarderProgram, initForwarderConfig, paState, program as paProgram, provider } from "./utils/adapterSuite";

describe("forwarder config (logic ref and direct-call guards)", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);
  const logicRef = randomRef();

  before(() => initForwarderConfig(logicRef, Keypair.generate().publicKey));

  describe("set_logic_ref", () => {
    // Mirrors the EVM forwarder's rotation on the upgradeable base
    // (ERC20ForwarderV2.reinitialize): the owner who authorizes upgrades
    // writes the new logic ref into the existing storage, and custody and
    // nonces are untouched. That owner is the upgrade authority the loader
    // records for this program.
    const rotate = (ref: number[], programData?: PublicKey) =>
      setLogicRef(forwarderProgram, provider.wallet.publicKey, ref, programData).rpc();

    it("rejects a zero logic ref", () =>
      assertFails(rotate(Array(32).fill(0)), { program: forwarderProgram, error: "ZeroAddressNotAllowed" }));

    it("rejects a signer that is not the program's upgrade authority", async () => {
      const impostor = Keypair.generate();
      await funder.fund(impostor, 1);
      await assertFails(setLogicRef(forwarderProgram, impostor.publicKey, randomRef()).signers([impostor]).rpc(), {
        program: forwarderProgram,
        error: "UnauthorizedCaller",
        account: "program_data",
      });
      assert.deepEqual(
        (await forwarderProgram.account.config.fetch(configPda)).logicRef,
        logicRef,
        "the config is untouched",
      );
    });

    // The upgrade-authority check reads whatever ProgramData is passed; the
    // program constraint pins it to this program's own. Another program with
    // the same upgrade authority is the cheapest forgery.
    it("rejects the upgrade authority of another program's ProgramData", () =>
      assertFails(rotate(randomRef(), deriveProgramDataPda(paProgram.programId)), {
        program: forwarderProgram,
        error: "UnauthorizedCaller",
        account: "program",
      }));

    it("rotates the logic ref in place and leaves the rest of the config untouched", async () => {
      const before = await forwarderProgram.account.config.fetch(configPda);
      const rotated = randomRef();

      const sig = await rotate(rotated);
      const after = await forwarderProgram.account.config.fetch(configPda);
      assert.deepEqual(after.logicRef, rotated, "the new logic ref is stored");
      assert.ok(after.protocolAdapter.equals(before.protocolAdapter), "the adapter is untouched");
      assert.ok(after.emergencyCommittee.equals(before.emergencyCommittee), "the committee is untouched");
      assert.ok(after.emergencyCaller.equals(before.emergencyCaller), "the emergency caller is untouched");

      const tx = await confirmedTransaction(provider.connection, sig);
      const parser = new anchor.EventParser(forwarderProgram.programId, forwarderProgram.coder);
      const events = [...parser.parseLogs(tx.meta!.logMessages!)];
      // pa-evm's rotation (ERC20ForwarderV2.reinitialize behind upgradeToAndCall)
      // emits only the proxy's Upgraded and Initialized events, nothing naming
      // the logic ref; the config account is where the new ref is read.
      assert.deepEqual(
        events.map((e) => e.name),
        [],
        "the rotation emits no forwarder event",
      );
    });
  });

  describe("guards", () => {
    // Mirrors ForwarderBase.t.sol: test_forwardCall_reverts_if_the_pa_is_not_the_caller.
    // The forwarder reads the current top-level instruction's program id from
    // the instructions sysvar; a direct call sees itself, not the adapter.
    it("rejects forward_call that is not a CPI from the adapter", () => {
      const operand = encodeUnwrapInput(Keypair.generate().publicKey, 1000n, Keypair.generate().publicKey);
      return assertFails(
        forwarderProgram.methods
          .forwardCall(logicRef, Buffer.concat([Buffer.from([OP_UNWRAP]), operand]))
          .accountsPartial({ config: configPda, ixSysvar: SYSVAR_INSTRUCTIONS_PUBKEY })
          .rpc(),
        { program: forwarderProgram, error: "UnauthorizedCaller" },
      );
    });

    // Mirrors EmergencyMigratableForwarderBase.t.sol: test_setEmergencyCaller_reverts_if_the_caller_is_not_the_emergency_committee
    it("rejects set_emergency_caller from a non-committee signer", async () => {
      const impostor = Keypair.generate();
      await funder.fund(impostor, 1);
      await assertFails(
        setEmergencyCaller(forwarderProgram, impostor.publicKey, paState, Keypair.generate().publicKey)
          .signers([impostor])
          .rpc(),
        { program: forwarderProgram, error: "UnauthorizedCaller" },
      );
    });
  });

  after(() => funder.drainAll());
});
