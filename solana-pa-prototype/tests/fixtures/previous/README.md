# Previous builds

The builds deployed on devnet before the in-place upgrade to this build, which
`tests/upgrade/` starts from. Each spec file `tests/upgrade/<program>.ts` runs
on its own validator, which loads `<program>.so` from here at that program's
id instead of this build (scripts/anchor-test.sh), upgrades the program in
place and migrates its accounts.

`protocol_adapter.so` is the adapter as deployed on devnet at
`2gH5DnTwDLLFk3oYxnbrceSFdTMtvaxQz29x5W8Etmpx` (protocol-adapter
2.0.0-rc.11, state schema version 4), dumped with
`solana program dump 2gH5DnTwDLLFk3oYxnbrceSFdTMtvaxQz29x5W8Etmpx` on
2026-11-16: the deterministic build of `contracts/v2.0.0-rc.11` (`7e1d46a`),
executable hash `7700a865e3c6040493ab5562ab64e6a3bcb259d5efee348d30e5e9dfabc20d2a`
(see docs/DEVNET_DEPLOYMENT.md), which `solana-verify get-executable-hash`
reports for the file. `protocol_adapter.json` is that release's production
IDL, `solana-pa-prototype/idl/protocol_adapter.json` at the tag; the test
calls the previous build's own instructions through it.

`batch_groth16_transfer_shape.json` (and `mock/` for the mock suite) is the
fixture the previous build settles before the upgrade: the fixture of that
name as of the change to schema 5, proven for arm-risc0 2.0.0-rc.8.2's
circuits, the real one by risc0 3.0.0 under selector `0x73c457ba`, which the
previous build stores. These are not regenerated with the set.

Replace a program's files with its deployed build whenever a release changes
one of its layouts, and the migrations and the upgrade test with them.
