/**
 * SPL token forwarder config initialization: the zero-value rejections and
 * what a successful initialize stores. Needs no config yet and touches
 * nothing of the adapter.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { assertRejects, deriveConfigPda, initializeForwarder, makeFunder, randomRef } from "./utils";
import { forwarderProgram, program as paProgram, provider } from "./utils/adapterSuite";

describe("forwarder initialize", () => {
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  const funder = makeFunder(provider);

  const authority = Keypair.generate();
  const emergencyCommittee = Keypair.generate();
  const logicRef = randomRef();

  before(() => funder.fund(authority, 2));

  const initialize = (adapter: PublicKey, ref: number[], committee: PublicKey) =>
    initializeForwarder(forwarderProgram, adapter, ref, committee, authority.publicKey).signers([authority]).rpc();

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

  after(() => funder.drainAll());
});
