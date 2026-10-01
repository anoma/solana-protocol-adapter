/**
 * update_expiry_config: the authority sets the TxData expiry bounds within
 * their limits.
 */
import * as anchor from "@anchor-lang/core";
import { assert } from "chai";
import { MIN_ALLOWED_EXPIRY, SEVEN_DAYS_SLOTS } from "../client/constants";
import { assertFails } from "./utils/helpers";
import { program, paState, setExpiryBounds, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (update_expiry_config)", () => {
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

  it("rejects min < MIN_ALLOWED_EXPIRY", async () => {
    await assertFails(setExpiryBounds(MIN_ALLOWED_EXPIRY - 1, 1000), { program, error: "InvalidExpiryConfig" });
  });

  it("rejects max > SEVEN_DAYS_SLOTS", async () => {
    await assertFails(setExpiryBounds(50, SEVEN_DAYS_SLOTS + 1), { program, error: "InvalidExpiryConfig" });
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
      { program, error: "Unauthorized" },
    );
  });

  it("restores the bounds the deployment held", async () => {
    await setExpiryBounds(foundMin, foundMax);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), foundMin, "min_expiry_slots is restored");
    assert.equal(state.maxExpirySlots.toNumber(), foundMax, "max_expiry_slots is restored");
  });
});
