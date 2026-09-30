/**
 * Each program answers `version` with the release it is, as pa-evm's adapter
 * and the EVM forwarder expose `VERSION`: read from the deployed program, it
 * names the code an address runs, which an in-place upgrade changes. The
 * release is the crate version, which the IDL also records.
 */
import { assert } from "chai";
import { program, forwarderProgram } from "./utils/adapterSuite";

describe("program versions", () => {
  for (const [name, target] of [
    ["protocol adapter", program],
    ["SPL token forwarder", forwarderProgram],
  ] as const) {
    it(`the ${name} answers with its release`, async () => {
      const version: string = await target.methods.version().view();
      assert.equal(version, target.idl.metadata.version, "the deployed program runs the release its IDL records");
    });
  }
});
