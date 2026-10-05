/**
 * The adapter answers `version` with the release it is, as pa-evm's adapter
 * exposes `VERSION`: read from the deployed program, it names the code an
 * address runs, which an in-place upgrade changes. The release is the crate
 * version, which the IDL also records.
 */
import { assert } from "chai";
import { program } from "./utils/adapterSuite";

describe("program versions", () => {
  it("the protocol adapter answers with its release", async () => {
    const version: string = await program.methods.version().view();
    assert.equal(version, program.idl.metadata.version, "the deployed program runs the release its IDL records");
  });
});
