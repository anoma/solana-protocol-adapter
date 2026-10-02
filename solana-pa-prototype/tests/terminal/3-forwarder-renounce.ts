/**
 * Renouncing the forwarder's ownership is for good, so it runs among the
 * suite's last files, before the committee's emergency and teardown files,
 * which need no owner and end with closing the config.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { reinitializeForwarder, upgradeForwarder } from "../../client/instructions";
import { localRenounceForwarderOwnership } from "../utils/localOnly";
import { assertFails } from "../utils/helpers";
import { cpiEventsOf, ensureForwarderConfig, forwarderProgram, provider, useAdapterSuite } from "../utils/adapterSuite";

describe("renounced forwarder ownership", () => {
  useAdapterSuite();
  const wallet = provider.wallet.publicKey;

  before(ensureForwarderConfig);

  // Mirrors OwnableUpgradeable.renounceOwnership on the EVM forwarder: the
  // owner becomes the zero address, announced, and neither the logic-ref
  // rotation nor an upgrade can run again.
  it("spl-token-forwarder (ownership): is renounced, announced, and closes reinitialize and upgrade", async () => {
    const sig = await localRenounceForwarderOwnership(forwarderProgram, wallet).rpc();
    const { events } = await cpiEventsOf(sig, forwarderProgram);
    assert.deepEqual(
      events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
      [["ownershipTransferred", wallet.toBase58(), PublicKey.default.toBase58()]],
    );
    await assertFails(reinitializeForwarder(forwarderProgram, wallet, Array(32).fill(1)).rpc(), {
      program: forwarderProgram,
      error: "OwnableUnauthorizedAccount",
      account: "authority",
    });
    await assertFails(upgradeForwarder(forwarderProgram, wallet, Keypair.generate().publicKey, wallet).rpc(), {
      program: forwarderProgram,
      error: "OwnableUnauthorizedAccount",
      account: "authority",
    });
  });
});
