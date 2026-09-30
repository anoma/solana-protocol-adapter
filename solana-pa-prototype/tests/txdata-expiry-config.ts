/**
 * update_expiry_config: the authority sets the TxData expiry bounds within
 * their limits.
 */
import * as anchor from "@anchor-lang/core";
import { assert } from "chai";
import { MAX_EXPIRY_SLOTS, MIN_ALLOWED_EXPIRY, MIN_EXPIRY_SLOTS, SEVEN_DAYS_SLOTS } from "../client/constants";
import { assertFails } from "./utils/helpers";
import { program, paState, setExpiryBounds, useAdapterSuite } from "./utils/adapterSuite";

describe("protocol-adapter (update_expiry_config)", () => {
  const { funder } = useAdapterSuite();

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

  it("restores default config", async () => {
    await setExpiryBounds(MIN_EXPIRY_SLOTS, MAX_EXPIRY_SLOTS);

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(
      state.minExpirySlots.toNumber(),
      MIN_EXPIRY_SLOTS,
      "min_expiry_slots should be restored to the default",
    );
    assert.equal(
      state.maxExpirySlots.toNumber(),
      MAX_EXPIRY_SLOTS,
      "max_expiry_slots should be restored to the default",
    );
  });
});
