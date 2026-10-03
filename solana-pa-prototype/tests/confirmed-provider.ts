// Every operator script and spec file sends and reads at `confirmed`, through
// client/provider.ts. Anchor's environment provider (`AnchorProvider.env`)
// defaults to `processed`:
// against a cluster RPC endpoint, which answers from several nodes, a blockhash
// fetched from one node at `processed` can be unknown to the node that
// simulates the transaction, which then fails with "Blockhash not found".
import { readdirSync, readFileSync, statSync } from "fs";
import { join, relative } from "path";
import { assert } from "chai";

const ROOT = join(__dirname, "..");
const PROVIDER_MODULE = "client/provider.ts";

/** Every .ts file under `dir`, relative to the project root. */
function typeScriptFiles(dir: string): string[] {
  return readdirSync(join(ROOT, dir)).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(join(ROOT, path)).isDirectory()) return typeScriptFiles(path);
    return path.endsWith(".ts") ? [path] : [];
  });
}

describe("every provider confirms", () => {
  it(`only ${PROVIDER_MODULE} builds a provider from Anchor's environment defaults`, () => {
    const offenders = ["client", "scripts", "tests"]
      .flatMap(typeScriptFiles)
      .filter((path) => relative(ROOT, join(ROOT, path)) !== PROVIDER_MODULE)
      .filter((path) => /AnchorProvider\.env\(\)/.test(readFileSync(join(ROOT, path), "utf8")));
    assert.deepEqual(offenders, [], "build the provider with client/provider.ts's confirmedProvider()");
  });
});
