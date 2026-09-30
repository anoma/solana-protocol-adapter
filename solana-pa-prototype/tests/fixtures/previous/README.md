# Previous builds

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
