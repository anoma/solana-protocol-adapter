/**
 * update_expiry_config: the authority sets the TxData expiry bounds within
 * their limits.
 */
import * as anchor from "@anchor-lang/core";
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import {
  SEVEN_DAYS_SLOTS,
  AUTHORITY_MISMATCH_PATTERN,
  errorHaystack,
} from "./utils";
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
    assert.equal(
      state.minExpirySlots.toNumber(),
      50,
      "min_expiry_slots should be 50"
    );
    assert.equal(
      state.maxExpirySlots.toNumber(),
      5000,
      "max_expiry_slots should be 5000"
    );
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

  it("rejects min < 10", async () => {
    try {
      await program.methods
        .updateExpiryConfig(new anchor.BN(5), new anchor.BN(1000))
        .accountsPartial({
          paState,
          authority: provider.wallet.publicKey,
        })
        .rpc();
      assert.fail("expected update_expiry_config with min < 10 to fail");
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
      assert.match(
        haystack,
        AUTHORITY_MISMATCH_PATTERN,
        `Expected authority constraint error, got: ${haystack}`
      );
    }
  });

  it("restores default config", async () => {
    await program.methods
      .updateExpiryConfig(new anchor.BN(100), new anchor.BN(216_000))
      .accountsPartial({
        paState,
        authority: provider.wallet.publicKey,
      })
      .rpc();

    const state = await program.account.paStateAccount.fetch(paState);
    assert.equal(state.minExpirySlots.toNumber(), 100, "min_expiry_slots should be restored to 100");
    assert.equal(state.maxExpirySlots.toNumber(), 216_000, "max_expiry_slots should be restored to 216_000");
  });
});
