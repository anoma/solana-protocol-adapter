/**
 * The adapter's owner is the program's upgrade authority, as pa-evm's owner
 * (OpenZeppelin's OwnableUpgradeable) is the one who authorizes its
 * upgrades: every owner-only instruction checks its signer against the
 * upgrade authority the loader records in the program's ProgramData, so
 * moving or renouncing the upgrade authority moves or renounces the
 * ownership. Every test but the last leaves the provider wallet as the
 * upgrade authority; the last renounces it.
 */
import { PublicKey, Transaction, TransactionInstruction } from "@solana/web3.js";
import { assert } from "chai";
import { EMPTY_KIND_TABLE_COMMITMENT } from "../client/constants";
import { pauseAdapter, setKindTableCommitment } from "../client/instructions";
import { BPF_LOADER_UPGRADEABLE, deriveProgramDataPda } from "../client/pda";
import { assertFails } from "./utils/helpers";
import { provider, program, paState, forwarderProgram, useAdapterSuite } from "./utils/adapterSuite";

/**
 * The loader's SetAuthority (instruction 4) on the adapter's ProgramData,
 * signed by `current`: `next` becomes the upgrade authority, or none when
 * `next` is null (the program is final).
 */
function setUpgradeAuthority(current: PublicKey, next: PublicKey | null): TransactionInstruction {
  return new TransactionInstruction({
    programId: BPF_LOADER_UPGRADEABLE,
    keys: [
      { pubkey: deriveProgramDataPda(program.programId), isSigner: false, isWritable: true },
      { pubkey: current, isSigner: true, isWritable: false },
      ...(next ? [{ pubkey: next, isSigner: false, isWritable: false }] : []),
    ],
    data: Buffer.from([4, 0, 0, 0]),
  });
}

describe("protocol-adapter (authority)", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  const kindTable = Array.from(EMPTY_KIND_TABLE_COMMITMENT);

  it("initializes unpaused", async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    assert.isFalse(state.paused);
  });

  it("rejects pause from a signer that is not the upgrade authority", async () => {
    const nonAuthority = await funder.fresh(1);
    await assertFails(pauseAdapter(program, nonAuthority.publicKey).signers([nonAuthority]).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
  });

  // The upgrade-authority check reads whatever ProgramData is passed, so the
  // account is pinned to this program's. Another program with the same
  // upgrade authority is the cheapest forgery.
  it("rejects the upgrade authority of another program's ProgramData", () =>
    assertFails(
      setKindTableCommitment(program, wallet, kindTable)
        .accountsPartial({ programData: deriveProgramDataPda(forwarderProgram.programId) })
        .rpc(),
      { program, error: "Unauthorized", account: "program_data" },
    ));

  // Mirrors OwnableUpgradeable.transferOwnership: the ownership moves with
  // the upgrade authority, at once.
  it("moves with the upgrade authority", async () => {
    const successor = await funder.fresh(1);
    await provider.sendAndConfirm(new Transaction().add(setUpgradeAuthority(wallet, successor.publicKey)));

    await assertFails(setKindTableCommitment(program, wallet, kindTable).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
    await setKindTableCommitment(program, successor.publicKey, kindTable).signers([successor]).rpc();

    await provider.sendAndConfirm(new Transaction().add(setUpgradeAuthority(successor.publicKey, wallet)), [successor]);
    await setKindTableCommitment(program, wallet, kindTable).rpc();
  });

  // Mirrors OwnableUpgradeable.renounceOwnership: with no upgrade authority
  // (the program final), every owner-only instruction is closed for good.
  it("is renounced with the upgrade authority", async () => {
    await provider.sendAndConfirm(new Transaction().add(setUpgradeAuthority(wallet, null)));
    await assertFails(setKindTableCommitment(program, wallet, kindTable).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
  });
});
