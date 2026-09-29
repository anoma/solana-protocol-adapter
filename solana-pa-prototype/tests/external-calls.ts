/**
 * External-call failures during settlement: a wrong CPI account, and the
 * test forwarder's failing and silent modes (localnet only: the test
 * forwarder is deployed nowhere else).
 */
import { Keypair } from "@solana/web3.js";
import { assert } from "chai";
import { loadFixture } from "./utils";
import {
  blockTimeForwarderId,
  testForwarderId,
  DUMMY_ROOT_MARKER,
  deriveNullifierAccounts,
  ensureAdapterInitialized,
  PA_ERRORS,
  extractPAErrorCode,
  assertPAError,
  useAdapterSuite,
} from "./utils/adapterSuite";

describe("protocol-adapter (External call error paths)", () => {
  const { settleFixtureViaTxData } = useAdapterSuite();

  before(async () => {
    await ensureAdapterInitialized();
  });

  it("rejects settlement when forwarder CPI accounts are wrong", async () => {
    // Use the mismatch fixture (valid proof, nonce=2 nullifiers not consumed).
    // Replace SYSVAR_CLOCK_PUBKEY with a random pubkey so the CPI to btf fails.
    // The inner CPI error propagates through (btf's AccountSysvarMismatch).
    const mismatchFixture = loadFixture("batch_groth16_mismatch.json");
    const payload = Buffer.from(mismatchFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(mismatchFixture.consumed_nullifiers_b64);
    const randomAccount = Keypair.generate().publicKey;
    const remainingAccounts = [
      ...nullifierAccounts,
      { pubkey: blockTimeForwarderId, isWritable: false, isSigner: false },
      { pubkey: randomAccount, isWritable: false, isSigner: false },
    ];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected CPI failure");
    } catch (e: any) {
      // CPI error propagation: Solana records the INNER program's error code,
      // not the PA's remapped ExternalCallCpiFailed. The PA's From<ProgramError>
      // impl runs in Rust but the runtime has already committed the inner code.
      // btf's AccountSysvarMismatch = Anchor error 3015 (0xBC7).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.notEqual(
        code,
        PA_ERRORS["ExternalCallCpiFailed"],
        "Solana CPI error propagation: inner error code should appear, not PA's remapped code",
      );
    }
  });

  it("rejects settlement when test-forwarder returns error", async () => {
    const failFixture = loadFixture("batch_forwarder_fail.json");
    const payload = Buffer.from(failFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(failFixture.consumed_nullifiers_b64);

    const remainingAccounts = [...nullifierAccounts, { pubkey: testForwarderId, isWritable: false, isSigner: false }];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected CPI failure from test-forwarder");
    } catch (e: any) {
      // CPI error propagation: test-forwarder's IntentionalFailure (6000)
      // propagates through instead of PA's ExternalCallCpiFailed (6019).
      const code = extractPAErrorCode(e);
      assert.isNotNull(code, "Expected a program error code in logs");
      assert.equal(code, 6000, "test-forwarder's IntentionalFailure (6000) should propagate through CPI");
    }
  });

  it("rejects ExternalCallOutputMismatch when forwarder returns no data", async () => {
    const silentFixture = loadFixture("batch_forwarder_silent.json");
    const payload = Buffer.from(silentFixture.tx_b64, "base64");
    const nullifierAccounts = deriveNullifierAccounts(silentFixture.consumed_nullifiers_b64);

    const remainingAccounts = [...nullifierAccounts, { pubkey: testForwarderId, isWritable: false, isSigner: false }];

    try {
      await settleFixtureViaTxData(payload, remainingAccounts, { newRootMarker: DUMMY_ROOT_MARKER });
      assert.fail("expected ExternalCallOutputMismatch error");
    } catch (e: any) {
      assertPAError(e, "ExternalCallOutputMismatch");
    }
  });
});
