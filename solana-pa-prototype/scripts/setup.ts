/**
 * Initializes the verifier router and registers the groth16 verifier.
 *
 * This script assumes programs are ALREADY DEPLOYED. It only performs:
 * 1. Router initialization (sets owner)
 * 2. Authority transfer (groth16_verifier authority -> router PDA)
 * 3. Verifier registration (adds groth16_verifier to router)
 *
 * ## Environment Variables
 *
 * - `RPC` - (Optional) RPC endpoint. Defaults to "http://localhost:8899"
 * - `RPC_SUBSCRIPTION` - (Optional) WebSocket endpoint. Defaults to "ws://localhost:8900"
 * - `ROUTER_ADDRESS` - (Required) Address of the deployed verifier_router program
 * - `VERIFIER_ADDRESS` - (Required) Address of the deployed groth16_verifier program
 * - `SELECTOR` - (Required) 4-byte hex selector for the verifier (e.g., "0x310fe598")
 *
 * ## Usage
 *
 * ```bash
 * ROUTER_ADDRESS=<router_pubkey> \
 * VERIFIER_ADDRESS=<verifier_pubkey> \
 * SELECTOR=0x310fe598 \
 * yarn ts-node scripts/setup.ts
 * ```
 */

import {
  createRpc,
  changeAuthority,
  getLocalKeypair,
  getRouterPda,
  createLogger,
  parseHex,
} from "../../risc0-solana/solana-verifier/scripts/utils/utils";
import { initializeRouter } from "../../risc0-solana/solana-verifier/scripts/utils/init";
import { addVerifier } from "../../risc0-solana/solana-verifier/scripts/utils/addVerifier";
import { address, Address } from "@solana/kit";

const logger = createLogger();

function getRequiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) {
    logger.fatal(`Missing required environment variable: ${name}`);
    process.exit(1);
  }
  return value;
}

async function runSetup(): Promise<void> {
  logger.info("Router setup script started (programs must be deployed already)");

  const routerAddress = address(getRequiredEnv("ROUTER_ADDRESS")) as Address<string>;
  const verifierAddress = address(getRequiredEnv("VERIFIER_ADDRESS")) as Address<string>;
  const selectorHex = getRequiredEnv("SELECTOR");
  const selector = parseHex(selectorHex, 4);

  const rpc = createRpc();
  const owner = await getLocalKeypair();

  logger.info(`Router: ${routerAddress}`);
  logger.info(`Verifier: ${verifierAddress}`);
  logger.info(`Selector: 0x${Buffer.from(selector).toString("hex")}`);
  logger.info(`Owner: ${owner.address}`);

  const routerPda = await getRouterPda(routerAddress);

  // Check if router is already initialized by checking if the PDA account exists
  try {
    const accountInfo = await rpc.rpc.getAccountInfo(routerPda.address, { encoding: "base64" }).send();
    if (accountInfo.value) {
      logger.info("Router already initialized, skipping setup");
      return;
    }
  } catch {
    // Account doesn't exist, proceed with initialization
  }

  // 1. Initialize the router
  logger.info("Step 1: Initializing router...");
  await initializeRouter(rpc.rpc, rpc.rpc_subscription, routerAddress, owner);

  // 2. Transfer verifier's upgrade authority to router PDA
  logger.info("Step 2: Transferring verifier authority to router PDA...");
  await changeAuthority(
    rpc.rpc,
    rpc.rpc_subscription,
    verifierAddress,
    owner,
    routerPda.address
  );

  // 3. Add verifier to router
  logger.info("Step 3: Adding verifier to router...");
  await addVerifier(
    rpc.rpc,
    rpc.rpc_subscription,
    verifierAddress,
    routerAddress,
    owner,
    selector
  );

  logger.info("Setup complete: Router initialized with verifier registered");
}

runSetup().catch((error) => {
  logger.error("Setup failed:", error);
  process.exit(1);
});
