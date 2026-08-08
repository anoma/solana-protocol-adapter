# Devnet Deployment Record

Live state is always what `./scripts/dev.sh status --cluster devnet` reports;
this file records what was deployed, by whom, and when. Update it after every
deploy, upgrade, estop, or teardown.

**Record last verified:** 2026-08-08

## Current deployment

| Program | ID | State |
|---|---|---|
| Protocol Adapter | `De5uxTic9Ed8dRW8TFDKDk6wWtCZa5BDCnLiVhLEoFyJ` | deployed, PAState initialized |
| Block Time Forwarder | `3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf` | deployed (stateless) |

- **Operator wallet** (PA authority and upgrade authority for both programs):
  `5S8LtbDPtQE7GtWWMBFY78gmiYp2LqS5BsoFDxKjoHr9`
  (`scripts/devnet-wallet.json`)

## Verifier infrastructure (pinned at initialize)

| Component | Address |
|---|---|
| RISC0 Verifier Router | `BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg` |
| Groth16 Verifier | `2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD` |
| Router PDA | `9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S` |
| Verifier Entry PDA (selector `0x73c457ba`) | `4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey` |

The local test validator clones the devnet verifier stack — see
`scripts/validator-deploy.sh` and `Anchor.toml` for the clone lists. The
router's on-chain owner is
`FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh` (this project's January
deployment wallet).

## Superseded deployments

| Program | ID | Owner wallet | Status |
|---|---|---|---|
| Protocol Adapter (2026-01-26) | `AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt` | `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh` | live, initialized, superseded — to be retired |
| Block Time Forwarder (2026-01-26) | `FLh2rbnAbtFZkLMMX36Fh4rV9wJWUFrLw5gDmoLzPEgq` | `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh` | live, superseded — to be retired |

Retirement procedure: `docs/OPERATIONS.md`, Sunsetting.
