/**
 * update_expiry_config: the authority sets the TxData expiry bounds within
 * their limits.
 */
import * as anchor from "@anchor-lang/core";
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { MAX_EXPIRY_SLOTS, MIN_ALLOWED_EXPIRY, MIN_EXPIRY_SLOTS, SEVEN_DAYS_SLOTS } from "../client/constants";
import { AUTHORITY_MISMATCH_PATTERN, errorHaystack } from "./utils";
import {
  provider,
  program,
  paState,
  ensureAdapterInitialized,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (update_expiry_config)", () => {
  const { funder } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  it("updates expiry config successfully", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), 50, "min_expiry_slots should be 50");
    assert.equal(state.maxExpirySlots.toNumber(), 5000, "max_expiry_slots should be 5000");
  });

  it("rejects min >= max", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(5000), new anchor.BN(100))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with min >= max to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects min < MIN_ALLOWED_EXPIRY", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(MIN_ALLOWED_EXPIRY - 1), new anchor.BN(1000))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with min < MIN_ALLOWED_EXPIRY to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects max > SEVEN_DAYS_SLOTS", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(50), new anchor.BN(SEVEN_DAYS_SLOTS + 1))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with max > SEVEN_DAYS_SLOTS to fail");
    } catch (e: any) {
      assertPAError(e, "InvalidExpiryConfig");
    }
  });

  it("rejects wrong authority", async () => {
    const nonAuthority = Keypair.generate();
    await funder.fund(nonAuthority, 1);

    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(50), new anchor.BN(5000))
        .accountsPartial({
          paState,
          authority: nonAuthority.publicKey,
        })
        .signers([nonAuthority])
        .rpc();
      assert.fail("expected update_expiry_config from wrong authority to fail");
    } catch (e: any) {
      const haystack = errorHaystack(e);
      assert.match(haystack, AUTHORITY_MISMATCH_PATTERN, `Expected authority constraint error, got: ${haystack}`);
    }
  });

  it("restores default config", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(MIN_EXPIRY_SLOTS), new anchor.BN(MAX_EXPIRY_SLOTS))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

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
