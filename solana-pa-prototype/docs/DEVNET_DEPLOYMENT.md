# Devnet Deployment Record

Live state is always what `./scripts/dev.sh status --cluster devnet` reports; this file records what was deployed, by whom, and when. Update it after every deploy, upgrade, estop, or teardown.

**Record last verified:** 2026-09-21

## Current deployment (V2)

| Program | ID | State |
|---|---|---|
| Protocol Adapter | `28Hvr1YFv2ouGN2fS99aF3ZzYXzkncJVVaHcZNhquLFT` | deployed 2026-09-09 from `anthony/arm-v2-port` `76648e6` (production build, `dev.sh deploy pa --cluster devnet`) in slot 495845894, signature `VPqWWoWTvp6RmCJjtg7skek1XFFsj5zLq2BPsJcc6jGwWxd5WwpXWQCoRpFZFfE4U3Mv3VQhQe8A9zBFstQbrP1`; PAState initialized with the empty kind table (`e3b0c442…`) and selector `0x73c457ba`; production IDL published on chain (IDL account `D6yXDGXaZYW13cz5L3VsAF89jjexMxiTh8DbYJf4DE5T`); upgraded 2026-09-16 from `anthony/settle-lookup-table` `30f13a4` (production build, `dev.sh upgrade pa --cluster devnet`, signature `1RWCxVagNbrndMMRdkGRXnQFPxY3SQiHwusvdgzHR1vHzbyUPcmXU1vX4EM8WgGUMBu2ti43zfJbnpoDfcMyJ6W`), IDL republished; upgraded 2026-09-21 from `anthony/arm-v2-port` `a275060` (production build on arm-risc0 v2.0.0-rc.6, `dev.sh upgrade pa --cluster devnet`; the program data was first extended by 10240 bytes, the loader's minimum, since the build is 240 bytes larger: signature `menEMJea4BefiUPq74qxXRNX8nRAQfVFy6mJbmorXUemxYFj6GfFF11nyRgc4QiFSFpQkukcYzrAt1f6v4b1Txg`; upgrade signature `iYA6pxjtmpeuHXsFES1aQez4FqUv3CJyLgDVVsfK2RHYb9ysgy6d3VyUH93M4i9GEBE2xd1XKtuCEaK1ZpkFeVz`, slot 502032375; IDL unchanged). Exercised 2026-09-21 by anoma-pa-solana-client's settle-fixture: `fixtures/devnet_v2_seed23.json`, proven locally for this build, settled as a v0 transaction through the lookup table (signature `4MfhZYndMgfY96RSjkK1CuqJbTW5QWj7hpe3mVdcoLY3cxEzgH3dXkXbuwSnczBbnn2agpH4zNWBGMfxgT5Xgxpo`, leaf 11) |
| Block Time Forwarder | `3mesRGxMv9wRB1xp7X4uxbf7GwnQC9PpHSJyCzcXwrsf` | deployed (stateless), shared across PA deployments |
| SPL Token Forwarder | `5CrHbBeHjg53UyL3Htn9dCYYTy68fMcrbDoeAdo4yQrx` | deployed 2026-09-18 from `anthony/anomapay-resource-v2` `02c842d` (production build, `dev.sh deploy stf --cluster devnet`, signature `5Xf1TC34mcdrs8S8mDAfEvdxbKBh4cbrFz4P5LEk8DDhJWVvayTyyHzviZ64fS2jwPHxYUGF9SZ2KeCfrSs1opdF`), upgraded the same day from `75ea413` (signature `61VY35n4Zbg9g1V7Rqu1nydzSn1T5m1BjYop7cknJ3NbPPAfdmAryvMxccGxmVZKNoP2FVSba79DSEeXvdwfZCgD`); config `8woJSGScukiScQsygq1boUZeuSriio748RrwNa8x7L5D` pins the adapter above, logic ref `725520e56ee1d2e6ec444788a87fd97aae15a9332386fdc360260345136a010b` (anoma/anomapay-solana-resource `f6b7e27`) and the operator wallet as emergency committee (the first config, under logic ref `7171ae21…`, was retired the same day by the runbook's rotation: `forwarder teardown` drained the escrow to the committee and closed the bitmaps and config, then `forwarder init` recreated them); escrow for the test mint `9EHEFzyuY7sZEzTVm7C3uMkNZFMgm5ZeWjGjirZ3MVfr` (6 decimals, mint authority = the fixtures' seeded user `AmJsDfsyQC18CncgXcqKBRCVhNyDhBpyUiQewm68zHBk`): PDA `9ESNoQepk1QBxtvHzNxB3xtNXBGWW22ds11cCntaZUCX`, ATA `FWZR9Yi6MMGG9mhzEJQP6CzjRifiCNiF2AeJKfUFz28q`. Exercised 2026-09-18 under each key: the committed `spl_token_wrap.json` settled through it from anoma-pa-solana-client's settle-fixture tool (under `7171ae21…`: `45oPZz48wDcfKCHvRJwuBggjkcvMyogRyso6L463HcctJqDdfEHHJe4PBZBWjwYi2f9mboLeDTVGQLf6Ajvnz3XW`; under `725520e5…`: `4WGqrtt5A1HkaaFDrpDwozPYms1GZamuzy57CZ5CguHp7rmdPCivCj5XaUceLMRVNyisYKGgYW2pLuKac8W7KUtQ`), 100 tokens in escrow, the user's word-0 nonce bitmap created in the settlement). Rotated 2026-09-21 to the resource's rc.6 key `dc1c0b4a70de90eec07ac4596172775c4d9ea2981e251dfab1fa862249042d91` (anoma/anomapay-solana-resource `963d6cb`) by the config re-initialization the runbook prescribed at the time: `drain-escrow` moved the 100 test tokens to the committee's ATA and closed the escrow ATA (`5MPL5Pf61aU7JTWStfrG5MJzGGLNw8VRCu2v6cCS3szkvRVxSuDrENxvbHjL794rMBoTxg5CjwxtPwuVJSvwRCeZ`), `close-config` (`3oAtxnwJyEQggLno6WKT3Dim8Mp5vch6VGwX24UJpQpqvBGeKqkEVxdRLdYo7cF2Lgu5j5iT9KZhzhqz4pWgFNTp`), `forwarder init` under the new key with the same committee (`4789jKAyhMnoFUWLqcJDojbFdgvkgpmyVdTrCkmf95mvVafWX5wNP43zFqFQu2We4EQW92YZ6rNSkknAFxUQUazj`), recreating the escrow ATA at its former address (`58QeM3t4N6zuUCqMx56VgRiyBe18baRp8JrN4HVkDv7RN6hGiFYYrXJ6Ba9N9WXDUwX2TRQrLVDprjVprwbiYogJ`); the nonce bitmaps were kept, so the committed wrap fixture's nonce stays used here. Upgraded 2026-09-21 from `anthony/arm-v2-port` `91b72c8` (production build with `set_logic_ref`, `dev.sh upgrade stf --cluster devnet`; program data extended by 69448 bytes first, signature `5xcDLuPCESBmiPDXuSobRxy9mRNcEAetA4VLaSYLBzTBhfpMxkAn4UnEZ4z9YBVBXRBjZieN9ULRCxkgu35fGkKi`; upgrade signature `38FAcEGA8hc2ChbLSp6oxtP2VtKGbne58TtekTkCzbhA6kVvjrWJTVYhFmyYNdPX9U1cxqaNVjDTaw7YeEjbLfxh`, slot 502073074; config untouched). Later rotations upgrade the program to a build that raises `CONFIG_VERSION`, then run `forwarder migrate` where the build changes a layout and `forwarder reinitialize` (OPERATIONS.md, "Rotating the logic ref") |

- **PAState PDA:** `9E8AZkYW1RN11iDJGQ2CQgqwGCaamqV4v5nZmhfqdPLn`
- **Settlement lookup table:** `CKAMrsJSf1SDgsmaM7hsKEwmi2efQsoCAuMNf4msGRSW` (created 2026-09-16 by the operator wallet, which remains its authority; signature `YkjCunBAEpBx3cRaifSaDZQTxX92Gv4SmgscF9XKhCg22nzFqruBuN5famsFe6tVwKXqBGyVDoeVVjjW84fFz2L`; 13 keys, the set `settlementLookupKeys` derived when it was created; it has since been extended with the forwarder's per-mint escrow authority and ATA for the test mint. The current derivation instead has a single escrow authority, a 14th fixed key, so the table needs that key once the forwarder's escrow is migrated). Extend it with each mint's escrow accounts when the forwarder is initialized for a mint (`lookup-table` with `STF_TOKEN_MINTS`). Shipped as `SETTLE_LOOKUP_TABLE` in anoma-pa-solana-client. Extended 2026-09-18 with the test mint's escrow authority and ATA (15 keys, signature `2PTom38dd941fSVrvTxYee1FHPy8ZP418qsNf57SVyf3rETU8PiAQQei6vsJqwuV4xCuwbkEu8Mr8EaA1i9kP7uC`). A second table, `7a5iMQcToZrRzBkx92tKjseznZt8bYvbdBiHttNb7LXJ`, was created by mistake the same day (the command was run without `PA_LOOKUP_TABLE`); it was deactivated and closed the same day, rent returned to the operator wallet.
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

The local test validator clones the devnet verifier stack — see `DEVNET_CLONE_PROGRAMS` and `DEVNET_CLONE_ACCOUNTS` in `scripts/validator-deploy.sh`. The router's on-chain owner is `FZjHgvuQsgKYRBnYWDAvFKP9L9MJpEnh4vgDcKFSWtGh` (this project's January deployment wallet).

## Retired deployments

All retired 2026-08-08 per the sunsetting procedure in `docs/OPERATIONS.md`. A retired PA's PAState account survives its program by design and can never be re-initialized.

| Program | ID | Outcome |
|---|---|---|
| PA (2026-01-26) | `AV1dFJCfq6CmJ523ft8YwNsQVYEoDxzNfEhEFJzoUkjt` | emergency-stopped (`4Jx7YAkTtRSFdyuRJpt7YnE7W845UsbGhtzwUjftvuf4cxUF4pKZhSoyf4B3pc4tahfHKVq9ouakjzJPgKMR6qVf`), program closed, 5.938 SOL reclaimed. Its build predates `close_markers_batch`, so ~0.056 SOL of marker/buffer/PAState rent is stranded. |
| BTF (2026-01-26) | `FLh2rbnAbtFZkLMMX36Fh4rV9wJWUFrLw5gDmoLzPEgq` | program closed, 1.321 SOL reclaimed |
| PA (pre-beta) | `De5uxTic9Ed8dRW8TFDKDk6wWtCZa5BDCnLiVhLEoFyJ` | emergency-stopped (`oAdcACnXzci89inxXozAW3U9BzweWomePRWaAeqWuQWRhwQQJjtEVvyB53depW8Lc2nXT6Tnwp6EomUZ2MSfTmm`), 16 markers closed (0.014 SOL), program closed, 4.789 SOL reclaimed |
| PA (rehearsal scratch, 2026-08-08) | `FU6kWfCF3pu7ZZU4Jep8HrZBTZpt7RXoApfPK5dPtcL7` | full lifecycle rehearsal: deployed (production build), initialized, settled the committed Groth16 fixtures via the cluster test subset, emergency-stopped (`2r27UmYrA4NZmxujFwPxiGsuDVVxzpMa3aA5wofsnVeQTthuLR27NzsynRJmanMsjHyYEu4N4nG8zUr8YGdEvzUA`), settlement rejection with `PAError::Stopped` observed on chain, torn down (3.977 SOL reclaimed; marker rent abandoned — production-build semantics) |
