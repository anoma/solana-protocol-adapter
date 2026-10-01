/**
 * The adapter authority, pa-evm's owner (OpenZeppelin's OwnableUpgradeable):
 * its record after initialize, the single-step transfer and the renouncement,
 * each announced with AuthorityTransferred. Every test but the last starts
 * and ends with the provider wallet as the authority; the last renounces.
 */
import { Keypair, PublicKey } from "@solana/web3.js";
import { assert } from "chai";
import { emergencyStop, renounceAuthority, setKindTableCommitment, transferAuthority } from "../client/instructions";
import { assertFails, randomRef } from "./utils/helpers";
import { provider, program, paState, cpiEventsOf, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (authority)", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;

  /** The AuthorityTransferred events of `sig`, as [previous, new] base58 pairs. */
  const transfersOf = async (sig: string) =>
    (await cpiEventsOf(sig)).events
      .filter((e) => e.name === "authorityTransferredEvent")
      .map((e) => [e.data.previousAuthority.toBase58(), e.data.newAuthority.toBase58()]);

  it("stores the initializing upgrade authority as the authority", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority.equals(wallet), "the provider wallet initialized the adapter");
  });

  it("initializes running", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(JSON.stringify(state.lifecycle), JSON.stringify({ running: {} }));
  });

  it("rejects emergency_stop from non-authority", async () => {
    const nonAuthority = await funder.fresh(1);
    await assertFails(emergencyStop(program, nonAuthority.publicKey).signers([nonAuthority]).rpc(), {
      program,
      error: "Unauthorized",
    });
  });

  // Mirrors OwnableUpgradeable.transferOwnership: onlyOwner.
  it("rejects a transfer by anyone but the authority", async () => {
    const nonAuthority = await funder.fresh(1);
    await assertFails(
      transferAuthority(program, nonAuthority.publicKey, Keypair.generate().publicKey).signers([nonAuthority]).rpc(),
      { program, error: "Unauthorized" },
    );
  });

  // Mirrors OwnableUpgradeable.transferOwnership: OwnableInvalidOwner(address(0)).
  it("rejects a transfer to the zero key", () =>
    assertFails(transferAuthority(program, wallet, PublicKey.default).rpc(), {
      program,
      error: "ZeroAuthorityNotAllowed",
    }));

  // A transfer takes effect at once, as pa-evm's single-step ownership
  // transfer does, and moves every authority power with it.
  it("transfers the authority at once and announces it", async () => {
    const successor = await funder.fresh(1);

    const sig = await transferAuthority(program, wallet, successor.publicKey).rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority.equals(successor.publicKey), "the successor is the authority");
    assert.deepEqual(await transfersOf(sig), [[wallet.toBase58(), successor.publicKey.toBase58()]]);
    await assertFails(emergencyStop(program, wallet).rpc(), { program, error: "Unauthorized" });

    await transferAuthority(program, successor.publicKey, wallet).signers([successor]).rpc();
    assert.ok((await program.account.paStateAccount.fetch(paState)).authority.equals(wallet), "restored");
  });

  // Mirrors OwnableUpgradeable.renounceOwnership: the owner becomes the zero
  // address, and every owner-only function is closed for good.
  it("renounces the authority for good", async () => {
    const sig = await renounceAuthority(program, wallet).rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.ok(state.authority.equals(PublicKey.default), "no one holds the authority");
    assert.deepEqual(await transfersOf(sig), [[wallet.toBase58(), PublicKey.default.toBase58()]]);
    await assertFails(setKindTableCommitment(program, wallet, randomRef()).rpc(), { program, error: "Unauthorized" });
  });
});
