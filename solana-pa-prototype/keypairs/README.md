# Test Keypairs

These keypairs are for **LOCAL TESTING ONLY** and ensure consistent program IDs across CI runs and fixture generation.

## Security Notice

These private keys are intentionally committed to the repository for test reproducibility. They must **NEVER** be used for mainnet or devnet deployments.

## Verifier Programs

The RISC0 verifier programs (router, groth16 verifier) are **downloaded from devnet** at test time rather than built from source. This ensures:
1. Tests use the exact same binaries as production
2. No need to maintain a submodule dependency
3. Faster CI builds (no verifier compilation)

See `scripts/download-devnet-verifiers.sh` for the download script.

## Protocol Adapter Keypairs

Located in `../target/deploy/`:
- `solana_pa_prototype-keypair.json`
- `block_time_forwarder-keypair.json`
- `spl_token_forwarder-keypair.json`

**Production:** Generate fresh keypairs at deployment time. Do not commit production keypairs to version control.

## Why Commit Test Keypairs?

Program IDs are derived from keypairs. Test fixtures reference specific program IDs, so consistent keypairs ensure:
1. Fixtures remain valid across CI runs
2. No 30+ minute fixture regeneration in CI
3. Deterministic test environments
