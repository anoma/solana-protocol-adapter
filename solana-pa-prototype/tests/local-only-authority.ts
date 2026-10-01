/**
 * Authority changes on a live cluster are made by hand, never by repository
 * code: the instructions that move or renounce an upgrade authority, or name
 * the forwarder's emergency caller, are built only by tests/utils/localOnly.ts,
 * which refuses any RPC endpoint that is not this machine's. On 2026-10-01 a
 * test pointed at devnet renounced the devnet adapter's upgrade authority.
 */
import { Connection, Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { localSetEmergencyCaller, localSetUpgradeAuthority } from "./utils/localOnly";
import { forwarderProgram, paState, program, provider, useAdapterSuite } from "./utils/adapterSuite";

const LIVE_ENDPOINTS = [
  "https://api.devnet.solana.com",
  "https://api.mainnet-beta.solana.com",
  "https://devnet.helius-rpc.com/?api-key=k",
  "http://10.0.0.5:8899",
  "http://127.0.0.1.example.com:8899",
];

describe("authority instructions are local-only", () => {
  useAdapterSuite();
  const someone = () => Keypair.generate().publicKey;

  for (const endpoint of LIVE_ENDPOINTS) {
    it(`refuses to build an upgrade-authority change against ${endpoint}`, () => {
      assert.throws(
        () => localSetUpgradeAuthority(new Connection(endpoint), program.programId, someone(), null),
        /local validator/,
      );
    });

    it(`refuses to build an emergency-caller change against ${endpoint}`, () => {
      assert.throws(
        () => localSetEmergencyCaller(new Connection(endpoint), forwarderProgram, someone(), paState, someone()),
        /local validator/,
      );
    });
  }

  it("builds both against the local validator", () => {
    assert.isOk(localSetUpgradeAuthority(provider.connection, program.programId, someone(), someone()));
    assert.isOk(localSetEmergencyCaller(provider.connection, forwarderProgram, someone(), paState, someone()));
  });
});
