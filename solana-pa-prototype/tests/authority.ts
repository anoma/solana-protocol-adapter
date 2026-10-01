/**
 * The adapter's owner is the program's upgrade authority, as pa-evm's owner
 * (OpenZeppelin's OwnableUpgradeable) is the one who authorizes its
 * upgrades: every owner-only instruction checks its signer against the
 * upgrade authority the loader records in the program's ProgramData, so
 * moving or renouncing the upgrade authority moves or renounces the
 * ownership. Every test leaves the provider wallet as the upgrade authority;
 * renouncing it is terminal/5-renounce.ts, among the suite's last files.
 */
import { Transaction } from "@solana/web3.js";
import { pauseAdapter, setKindTableCommitment } from "../client/instructions";
import { deriveProgramDataPda } from "../client/pda";
import { localSetUpgradeAuthority } from "./utils/localOnly";
import { assertFails } from "./utils/helpers";
import { provider, program, paState, forwarderProgram, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (authority) @localnet", () => {
  const { funder } = useAdapterSuite();
  const wallet = provider.wallet.publicKey;
  // The owner-only call these tests make: rewriting the kind table the
  // adapter already holds, which leaves its configuration as it was found.
  let kindTable: number[];
  before(async () => {
    kindTable = Array.from((await program.account.paStateAccount.fetch(paState)).kindTableCommitment);
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
    await provider.sendAndConfirm(
      new Transaction().add(
        localSetUpgradeAuthority(provider.connection, program.programId, wallet, successor.publicKey),
      ),
    );

    await assertFails(setKindTableCommitment(program, wallet, kindTable).rpc(), {
      program,
      error: "Unauthorized",
      account: "program_data",
    });
    await setKindTableCommitment(program, successor.publicKey, kindTable).signers([successor]).rpc();

    await provider.sendAndConfirm(
      new Transaction().add(
        localSetUpgradeAuthority(provider.connection, program.programId, successor.publicKey, wallet),
      ),
      [successor],
    );
    await setKindTableCommitment(program, wallet, kindTable).rpc();
  });
});
