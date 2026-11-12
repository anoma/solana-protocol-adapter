/**
 * update_expiry_config: the authority sets the TxData expiry bounds within
 * their limits.
 */
import * as anchor from "@anchor-lang/core";
import { assert } from "chai";
import { assertFails } from "./utils/helpers";
import { program, paState, setExpiryBounds, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (update_expiry_config) @localnet", () => {
  const { funder } = useAdapterSuite();

  // The bounds the deployment held; the last test restores them.
  let foundMin: number;
  let foundMax: number;
  before(async () => {
    const state = await program.account.paStateAccount.fetch(paState);
    foundMin = state.minExpirySlots.toNumber();
    foundMax = state.maxExpirySlots.toNumber();
  });

  it("updates expiry config successfully", async () => {
    await setExpiryBounds(50, 5000);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), 50, "min_expiry_slots should be 50");
    assert.equal(state.maxExpirySlots.toNumber(), 5000, "max_expiry_slots should be 5000");
  });

  it("rejects min >= max", async () => {
    await assertFails(setExpiryBounds(5000, 100), { program, error: "InvalidExpiryConfig" });
  });

  it("rejects a minimum below 10 slots", async () => {
    await assertFails(setExpiryBounds(9, 1000), { program, error: "InvalidExpiryConfig" });
  });

  it("rejects a maximum above seven days of 400 ms slots (1,512,000)", async () => {
    await assertFails(setExpiryBounds(50, 1_512_001), { program, error: "InvalidExpiryConfig" });
  });

  it("rejects wrong authority", async () => {
    const nonAuthority = await funder.fresh(1);

    await assertFails(
      program.methods
        .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc(),
      { program, error: "OwnableUnauthorizedAccount" },
    );
  });

  it("restores the bounds the deployment held", async () => {
    await setExpiryBounds(foundMin, foundMax);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), foundMin, "min_expiry_slots is restored");
    assert.equal(state.maxExpirySlots.toNumber(), foundMax, "max_expiry_slots is restored");
  });
});
