// An operator script for tests/cli-utils.ts: its main finishes (or throws, given
// "reject") while a handle stays open, as web3.js's reconnecting websocket does.
import { runScript } from "../../scripts/cli-utils";

setInterval(() => {}, 1000);
runScript("the child failed", async () => {
  console.log("done");
  if (process.argv[2] === "reject") throw new Error("boom");
});
