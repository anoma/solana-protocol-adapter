/**
 * The adapter's owner, as pa-evm's (OpenZeppelin's OwnableUpgradeable): it is
 * stored in the state, signs every owner-only instruction, moves with
 * `transfer_ownership`, and alone upgrades the program, whose upgrade
 * authority is the program's own PDA, as pa-evm's UUPS implementation
 * authorizes its own upgrades. Every test leaves the provider wallet as the
 * owner and the deployed code as it found it; renouncing the ownership is
 * terminal/6-renounce.ts, the suite's last file.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { readFileSync } from "fs";
import { pauseAdapter, setKindTableCommitment, upgradeAdapter } from "../client/instructions";
import { BPF_LOADER_UPGRADEABLE } from "../client/pda";
import { deployedExecutableHash, executableHash } from "../client/upgrade";
import { localTransferAdapterOwnership } from "./utils/localOnly";
import { assertFails, solanaCli, waitForSlotPast, writeBuffer } from "./utils/helpers";
import { cpiEventsOf, provider, program, paState, useAdapterSuite } from "./utils/adapterSuite";

const ADAPTER_SO = "target/deploy/protocol_adapter.so";

describe("protocol-adapter (ownership and upgrades) @localnet", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  // The owner-only call these tests make: rewriting the kind table the
  // adapter already holds, which leaves its configuration as it was found.
  let kindTable: number[];
  before(async () => {
    kindTable = Array.from((await program.account.paStateAccount.fetch(paState)).kindTableCommitment);
  });

  it("rejects pause from a signer that is not the owner", async () => {
    const stranger = await funder.fresh(1);
    await assertFails(pauseAdapter(program, stranger.publicKey).signers([stranger]).rpc(), {
      program,
      error: "OwnableUnauthorizedAccount",
      account: "authority",
    });
  });

  describe("transfer_ownership", () => {
    it("rejects a signer that is not the owner", async () => {
      const stranger = await funder.fresh(1);
      await assertFails(
        localTransferAdapterOwnership(program, stranger.publicKey, stranger.publicKey).signers([stranger]).rpc(),
        { program, error: "OwnableUnauthorizedAccount", account: "authority" },
      );
    });

    // OwnableUpgradeable.transferOwnership: OwnableInvalidOwner(address(0)).
    it("rejects the zero key", () =>
      assertFails(localTransferAdapterOwnership(program, wallet, PublicKey.default).rpc(), {
        program,
        error: "OwnableInvalidOwner",
      }));

    // OwnableUpgradeable.transferOwnership: the ownership moves at once,
    // announced with OwnershipTransferred(previous, new).
    it("moves the ownership at once and announces it", async () => {
      const successor = await funder.fresh(1);
      const sig = await localTransferAdapterOwnership(program, wallet, successor.publicKey).rpc();
      const { events } = await cpiEventsOf(sig);
      assert.deepEqual(
        events.map((e) => [e.name, e.data.previousOwner.toBase58(), e.data.newOwner.toBase58()]),
        [["ownershipTransferredEvent", wallet.toBase58(), successor.publicKey.toBase58()]],
      );

      await assertFails(setKindTableCommitment(program, wallet, kindTable).rpc(), {
        program,
        error: "OwnableUnauthorizedAccount",
        account: "authority",
      });
      await setKindTableCommitment(program, successor.publicKey, kindTable).signers([successor]).rpc();

      await localTransferAdapterOwnership(program, successor.publicKey, wallet).signers([successor]).rpc();
      await setKindTableCommitment(program, wallet, kindTable).rpc();
    });
  });

  describe("upgrade", () => {
    it("rejects a signer that is not the owner", async () => {
      const stranger = await funder.fresh(1);
      await assertFails(
        upgradeAdapter(program, stranger.publicKey, Keypair.generate().publicKey, stranger.publicKey)
          .signers([stranger])
          .rpc(),
        { program, error: "OwnableUnauthorizedAccount", account: "authority" },
      );
    });

    it("rejects an account that is not a loader buffer", () =>
      assertFails(upgradeAdapter(program, wallet, paState, wallet).rpc(), {
        program,
        error: "InvalidUpgradeBuffer",
      }));

    // The loader requires the buffer's authority to match the program's: the
    // program hands the owner's buffer to its PDA, so a buffer someone other
    // than the owner wrote is refused there.
    it("rejects a buffer someone other than the owner wrote", async () => {
      const successor = await funder.fresh(1);
      const buffer = writeBuffer(provider, ADAPTER_SO);
      await localTransferAdapterOwnership(program, wallet, successor.publicKey).rpc();
      await assertFails(
        upgradeAdapter(program, successor.publicKey, buffer, successor.publicKey).signers([successor]).rpc(),
        { program: BPF_LOADER_UPGRADEABLE, reason: /^Incorrect authority provided$/ },
      );
      await localTransferAdapterOwnership(program, successor.publicKey, wallet).signers([successor]).rpc();
      solanaCli(provider, "program", "close", buffer.toBase58(), "--bypass-warning");
    });

    // UUPS upgradeToAndCall: the owner replaces the code, announced with
    // ERC1967's Upgraded, here naming the code by its executable hash. The
    // suite upgrades to the build it already runs, which leaves the
    // deployment as it was.
    it("replaces the code with the owner's buffer, announces its executable hash, and runs it from the next slot", async () => {
      const expected = executableHash(readFileSync(ADAPTER_SO));
      const buffer = writeBuffer(provider, ADAPTER_SO);
      const spill = Keypair.generate().publicKey;
      const bufferRent = (await provider.connection.getAccountInfo(buffer))!.lamports;

      const sig = await upgradeAdapter(program, wallet, buffer, spill).rpc();
      const { tx, events } = await cpiEventsOf(sig);
      assert.deepEqual(
        events.map((e) => [e.name, Buffer.from(e.data.executableHash).toString("hex")]),
        [["upgradedEvent", expected.toString("hex")]],
        "upgrade announces the buffer's executable hash",
      );
      assert.deepEqual(
        await deployedExecutableHash(provider.connection, program.programId),
        expected,
        "the program runs the buffer's code",
      );
      assert.isNull(await provider.connection.getAccountInfo(buffer), "the loader closes the buffer");
      assert.equal(await provider.connection.getBalance(spill), bufferRent, "the buffer's rent goes to spill");

      await waitForSlotPast(provider.connection, tx.slot);
      await setKindTableCommitment(program, wallet, kindTable).rpc();
    });
  });
});
