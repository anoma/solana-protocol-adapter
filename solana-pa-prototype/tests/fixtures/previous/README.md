# Previous builds

`protocol_adapter.so` is the adapter as deployed on devnet before the
in-place upgrade to this build (state schema version 1), dumped with
`solana program dump --url devnet 28Hvr1YFv2ouGN2fS99aF3ZzYXzkncJVVaHcZNhquLFT`
on 2026-09-30:

- deployed in slot 502032375 from `anthony/arm-v2-port` `a275060`
  (see docs/DEVNET_DEPLOYMENT.md);
- sha256 `42ebc170db7c0d026a3eec6f5e9c1fbf6f67c8c7e962ee64f3a61dc50654f873`.

`tests/adapter-upgrade.ts` starts on it, upgrades the program to this build
and migrates the state (`migrate_state`).

`spl_token_forwarder.so` is the SPL token forwarder as deployed on devnet
before the in-place upgrade to this build, dumped with
`solana program dump --url devnet 5CrHbBeHjg53UyL3Htn9dCYYTy68fMcrbDoeAdo4yQrx`
on 2026-09-30:

- deployed in slot 502073074 from `anthony/arm-v2-port` `91b72c8`
  (see docs/DEVNET_DEPLOYMENT.md);
- sha256 `b5d1dbeed5e050ae69c1d732a256fa61026f3faec2c07e607df87bed52dd510c`.

`tests/forwarder-upgrade.ts` starts its validator with this build at the
forwarder's id (scripts/anchor-test.sh `PREVIOUS_BUILD_SPECS`), creates
accounts through it, upgrades the program to this build and migrates them.
Replace it with the deployed build whenever a release changes a layout, and
update the migrations with it.
