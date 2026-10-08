// No code builds a provider from Anchor's environment defaults (`processed`)
// except client/provider.ts, which confirms at `confirmed` (why: its header).
import { readdirSync, readFileSync } from "fs";
import { join } from "path";
import { assert } from "chai";

const ROOT = join(__dirname, "..");
const PROVIDER_MODULE = "client/provider.ts";

/** Every .ts file under `dir`, relative to the project root. */
const typeScriptFiles = (dir: string): string[] =>
  readdirSync(join(ROOT, dir), { recursive: true, encoding: "utf8" })
    .filter((path) => path.endsWith(".ts"))
    .map((path) => join(dir, path));

describe("every provider confirms", () => {
  it(`only ${PROVIDER_MODULE} builds a provider from Anchor's environment defaults`, () => {
    const sources = ["client", "scripts", "tests"].flatMap(typeScriptFiles).filter((path) => path !== PROVIDER_MODULE);
    assert.isAbove(sources.length, 0, "found no TypeScript sources to check");
    const offenders = sources.filter((path) => /AnchorProvider\.env\(\)/.test(readFileSync(join(ROOT, path), "utf8")));
    assert.deepEqual(offenders, [], "build the provider with client/provider.ts's confirmedProvider()");
  });
});
