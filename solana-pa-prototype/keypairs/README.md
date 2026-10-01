# Program Keypairs

The program keypairs under `../target/deploy/` are committed so that every checkout builds the same program IDs: local tests, CI, fixture generation (fixtures embed program IDs), and the devnet deployments recorded in `docs/DEVNET_DEPLOYMENT.md`.

- `protocol_adapter-keypair.json`
- `spl_token_forwarder-keypair.json`
- `block_time_forwarder-keypair.json`
- `mock_verifier-keypair.json` (localnet only)
- `test_forwarder-keypair.json` (localnet only)

## Security Notice

These private keys are public. Anyone can deploy a program to one of these IDs on a cluster where it is not yet deployed, so never use them for mainnet: a mainnet deployment uses keypairs generated for it and kept out of version control. Program keypairs only claim an ID at first deploy; after that, the program's upgrade authority, not the program keypair, controls it.

## Verifier Programs

The RISC0 verifier programs (router, Groth16 verifier) are copied from devnet rather than built here, so tests run the deployed binaries: `fetch_devnet_clones` in `scripts/validator-deploy.sh` downloads them, and `start_validator` loads them at genesis.
