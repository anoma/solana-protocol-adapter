/**
 * The SPL token forwarder's owner, as the EVM V2 forwarder's
 * (OwnableUpgradeable): it is stored in the config, rotates the logic ref,
 * moves with `transfer_ownership`, and alone upgrades the program, whose
 * upgrade authority is the program's own PDA. Every test leaves the
 * provider wallet as the owner and the deployed code as it found it;
 * renouncing the ownership is terminal/3-forwarder-renounce.ts.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { reinitializeForwarder, upgradeForwarder } from "../client/instructions";
import { BPF_LOADER_UPGRADEABLE, deriveConfigPda } from "../client/pda";
import { localTransferForwarderOwnership } from "./utils/localOnly";
import { FORWARDER_SO } from "./utils/constants";
import { assertFails, randomRef, solanaCli, writeBuffer } from "./utils/helpers";
import {
  cpiEventsOf,
  ensureForwarderConfig,
  forwarderProgram,
  provider,
  upgradeThroughProgram,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("forwarder ownership and upgrades @localnet", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const [configPda] = deriveConfigPda(forwarderProgram.programId);
  // The owner-only call these tests make: reinitialize, which a config at
  // this build's version refuses with InvalidInitialization after the owner
  // check, so it changes nothing.
  const reinitializeAs = (owner: Keypair | null) =>
    reinitializeForwarder(forwarderProgram, owner?.publicKey ?? wallet, randomRef())
      .signers(owner ? [owner] : [])
      .rpc();

  before(ensureForwarderConfig);

  describe("transfer_ownership", () => {
    it("rejects a signer that is not the owner", async () => {
      const stranger = await funder.fresh(1);
      await assertFails(
        localTransferForwarderOwnership(forwarderProgram, stranger.publicKey, stranger.publicKey)
          .signers([stranger])
          .rpc(),
        { program: forwarderProgram, error: "OwnableUnauthorizedAccount", account: "authority" },
      );
    });

    // OwnableUpgradeable.transferOwnership: OwnableInvalidOwner(address(0)).
    it("rejects the zero key", () =>
      assertFails(localTransferForwarderOwnership(forwarderProgram, wallet, PublicKey.default).rpc(), {
        program: forwarderProgram,
        error: "OwnableInvalidOwner",
      }));

    // OwnableUpgradeable.transferOwnership: the ownership moves at once,
    // announced with OwnershipTransferred(previous, new).
    it("moves the ownership at once and announces it", async () => {
      const successor = await funder.fresh(1);
      const sig = await localTransferForwarderOwnership(forwarderProgram, wallet, successor.publicKey).rpc();
      const { events } = await cpiEventsOf(sig, forwarderProgram);
      assert.deepEqual(
        events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
        [["ownershipTransferred", wallet.toBase58(), successor.publicKey.toBase58()]],
      );
      assert.equal(
        (await forwarderProgram.account.config.fetch(configPda)).owner.toBase58(),
        successor.publicKey.toBase58(),
      );

      await assertFails(reinitializeAs(null), {
        program: forwarderProgram,
        error: "OwnableUnauthorizedAccount",
        account: "authority",
      });
      await assertFails(reinitializeAs(successor), { program: forwarderProgram, error: "InvalidInitialization" });

      await localTransferForwarderOwnership(forwarderProgram, successor.publicKey, wallet).signers([successor]).rpc();
    });
  });

  describe("upgrade", () => {
    it("rejects a signer that is not the owner", async () => {
      const stranger = await funder.fresh(1);
      await assertFails(
        upgradeForwarder(forwarderProgram, stranger.publicKey, Keypair.generate().publicKey, stranger.publicKey)
          .signers([stranger])
          .rpc(),
        { program: forwarderProgram, error: "OwnableUnauthorizedAccount", account: "authority" },
      );
    });

    it("rejects an account that is not a loader buffer", () =>
      assertFails(upgradeForwarder(forwarderProgram, wallet, configPda, wallet).rpc(), {
        program: forwarderProgram,
        error: "InvalidUpgradeBuffer",
      }));

    // The program hands the owner's buffer to its PDA, so a buffer someone
    // other than the owner wrote is refused by the loader.
    it("rejects a buffer someone other than the owner wrote", async () => {
      const successor = await funder.fresh(1);
      const buffer = writeBuffer(provider, FORWARDER_SO);
      await localTransferForwarderOwnership(forwarderProgram, wallet, successor.publicKey).rpc();
      await assertFails(
        upgradeForwarder(forwarderProgram, successor.publicKey, buffer, successor.publicKey).signers([successor]).rpc(),
        { program: BPF_LOADER_UPGRADEABLE, reason: /^Incorrect authority provided$/ },
      );
      await localTransferForwarderOwnership(forwarderProgram, successor.publicKey, wallet).signers([successor]).rpc();
      solanaCli(provider, "program", "close", buffer.toBase58(), "--bypass-warning");
    });

    // UUPS upgradeToAndCall: the owner replaces the code, announced with
    // ERC1967's Upgraded, naming the code by its executable hash. The suite
    // upgrades to the build it already runs.
    it("replaces the code with the owner's buffer, announces its executable hash, and runs it from the next slot", async () => {
      await upgradeThroughProgram(forwarderProgram, FORWARDER_SO, "upgraded", (buffer, spill) =>
        upgradeForwarder(forwarderProgram, wallet, buffer, spill),
      );
      await assertFails(reinitializeAs(null), { program: forwarderProgram, error: "InvalidInitialization" });
    });
  });
});
