# Devnet Deployment Record

Live state is always what `./scripts/dev.sh status --cluster devnet` reports; this file records what was deployed, by whom, and when. Update it after every deploy, upgrade, estop, or teardown.

**Record last verified:** 2026-09-16

## Current deployment (V2)

| Program | ID | State |
|---|---|---|
| Protocol Adapter | `28Hvr1YFv2ouGN2fS99aF3ZzYXzkncJVVaHcZNhquLFT` | deployed 2026-09-09 from `anthony/arm-v2-port` `76648e6` (production build, `dev.sh deploy pa --cluster devnet`) in slot 495845894, signature `VPqWWoWTvp6RmCJjtg7skek1XFFsj5zLq2BPsJcc6jGwWxd5WwpXWQCoRpFZFfE4U3Mv3VQhQe8A9zBFstQbrP1`; PAState initialized with the empty kind table (`e3b0c442…`) and selector `0x73c457ba`; production IDL published on chain (IDL account `D6yXDGXaZYW13cz5L3VsAF89jjexMxiTh8DbYJf4DE5T`); upgraded 2026-09-16 from `anthony/settle-lookup-table` `30f13a4` (production build, `dev.sh upgrade pa --cluster devnet`, signature `1RWCxVagNbrndMMRdkGRXnQFPxY3SQiHwusvdgzHR1vHzbyUPcmXU1vX4EM8WgGUMBu2ti43zfJbnpoDfcMyJ6W`), IDL republished |
| Block Time Forwarder | `3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf` | deployed (stateless), shared across PA deployments |

- **PAState PDA:** `9E8AZkYW1RN11iDJGQ2CQgqwGCaamqV4v5nZmhfqdPLn`
- **Settlement lookup table:** `CKAMrsJSf1SDgsmaM7hsKEwmi2efQsoCAuMNf4msGRSW` (created 2026-09-16 by the operator wallet, which remains its authority; signature `YkjCunBAEpBx3cRaifSaDZQTxX92Gv4SmgscF9XKhCg22nzFqruBuN5famsFe6tVwKXqBGyVDoeVVjjW84fFz2L`; 13 keys, the set `settlementLookupKeys` derives and `docs/OPERATIONS.md` lists). Extend it with each mint's escrow accounts when the forwarder is initialized for a mint (`lookup-table` with `STF_TOKEN_MINTS`). Shipped as `SETTLE_LOOKUP_TABLE` in anoma-pa-solana-client.
- **Operator wallet** (PA authority, upgrade authority, IDL authority): `5S8LtbDPtQE7GtWWMBFY78gmiYp2LqS5BsoFDxKjoHr9` (`scripts/devnet-wallet.json`)
- **Program keypair:** `target/deploy/protocol_adapter-keypair.json` on this branch (needed only for the first deploy)
- **Indexing:** the Solana Envio project in anoma-envio (`solana/config.yaml`) indexes this program from slot 495845894.

The V1 beta below keeps serving V1 clients from `main`; this deployment does not touch it.

## V1 beta deployment (main)

| Program | ID | State |
|---|---|---|
| Protocol Adapter | `9hDoEFv9hyECfruQUxDetE8CiuFrB2fbCiHuD5GUKFeF` | deployed 2026-08-08 (production build), PAState initialized; production IDL published on chain 2026-08-11 as `protocol_adapter` — explorers display "Protocol Adapter" (IDL account `AoEQQEXZQURXh8jpwKZLaQ7woStZaEZuMPthZxqjwJYv`); exercised in place 2026-08-10: full cluster suite (59 passing) settled the committed Groth16 fixtures against it (8 marker accounts on chain, authority verified back with the operator wallet); upgraded 2026-08-11 to the deterministic `solana-verify` build (deployed hash `070d5b63ef99ce291e35916f783618f55284db9cb1b527846935d8ff97745827` = the reproducible build of this repo; check with `dev.sh verify-build --cluster devnet` or `solana-verify verify-from-repo`) |
| Block Time Forwarder | `3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf` | deployed (stateless), shared across PA deployments |

- **PAState PDA:** `FrpGojQ5LKxY5afjRgBEL1fENy5GrjbzdwHLFWkf97ui`
- **Operator wallet** (PA authority and upgrade authority for both programs): `5S8LtbDPtQE7GtWWMBFY78gmiYp2LqS5BsoFDxKjoHr9` (`scripts/devnet-wallet.json`)

## Verifier infrastructure (pinned at initialize)

| Component | Address |
|---|---|
| RISC0 Verifier Router | `BetEAE4npinksQBxvqUN1KkCVjYFJywWao45MSWtp5yg` |
| Groth16 Verifier | `2Yfa83Lzbn71ie3J1KQRiNQz1qHnvVm8gkBCpXZQ7ajD` |
| Router PDA | `9ZJmYSYaYq38GfwQMsEw5gkzfr94Vbzw6Nv53yQuCv2S` |
| Verifier Entry PDA (selector `0x73c457ba`) | `4ktbrXwBXZMoND5qb3J6abS1m8KqwUtCjjDBebJ4vqey` |

The local test validator clones the devnet verifier stack — see `scripts/validator-deploy.sh` and `Anchor.toml` for the clone lists. The router's on-chain owner is `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh` (this project's January deployment wallet).

## Retired deployments

All retired 2026-08-08 per the sunsetting procedure in `docs/OPERATIONS.md`. A retired PA's PAState account survives its program by design and can never be re-initialized.

| Program | ID | Outcome |
|---|---|---|
| PA (2026-01-26) | `AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt` | emergency-stopped (`4Jx7YAkTtRSFdyuRJpt7YnE7W845UsbGhtzwUjftvuf4cxUF4pKZhSoyf4B3pc4tahfHKVq9ouakjzJPgKMR6qVf`), program closed, 5.938 SOL reclaimed. Its build predates `close_markers_batch`, so ~0.056 SOL of marker/buffer/PAState rent is stranded. |
| BTF (2026-01-26) | `FLh2rbnAbtFZkLMMX36Fh4rV9wJWUFrLw5gDmoLzPEgq` | program closed, 1.321 SOL reclaimed |
| PA (pre-beta) | `De5uxTic9Ed8dRW8TFDKDk6wWtCZa5BDCnLiVhLEoFyJ` | emergency-stopped (`oAdcACnXzci89inxXozAW3U9BzweWomePRWaAeqWuQWRhwQQJjtEVvyB53depW8Lc2nXT6Tnwp6EomUZ2MSfTmm`), 16 markers closed (0.014 SOL), program closed, 4.789 SOL reclaimed |
| PA (rehearsal scratch, 2026-08-08) | `FU6kWfCF3pu7ZZU4Jep8HrZBTZpt7RXoApfPK5dPtcL7` | full lifecycle rehearsal: deployed (production build), initialized, settled the committed Groth16 fixtures via the cluster test subset, emergency-stopped (`2r27UmYrA4NZmxujFwPxiGsuDVVxzpMa3aA5wofsnVeQTthuLR27NzsynRJmanMsjHyYEu4N4nG8zUr8YGdEvzUA`), settlement rejection with `PAError::Stopped` observed on chain, torn down (3.977 SOL reclaimed; marker rent abandoned — production-build semantics) |
