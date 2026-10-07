# Previous builds

The builds deployed on devnet before the in-place upgrade to this build, which
`tests/upgrade/` starts from. Each spec file `tests/upgrade/<program>.ts` runs
on its own validator, which loads `<program>.so` from here at that program's
id instead of this build (scripts/anchor-test.sh), upgrades the program in
place and migrates its accounts.

`protocol_adapter.so` is the adapter as deployed on devnet at
`5zeqkB3kc9fd1RvaXB2GeMB53Jgf98QJtaFK38e6tTsc` (protocol-adapter
2.0.0-rc.3, state schema version 3), dumped with
`solana program dump 5zeqkB3kc9fd1RvaXB2GeMB53Jgf98QJtaFK38e6tTsc` on
2026-10-07: the deterministic build of `anthony/arm-v2-port` `bc53d70`,
executable hash `2d193a0d3810aedd481b1cbaea7f3ac07a41b9426415675efc7d3c6b479defd5`
(see docs/DEVNET_DEPLOYMENT.md), which `solana-verify get-executable-hash`
reports for the file. `protocol_adapter.json` is its production IDL, fetched
with `anchor idl fetch` from its Program Metadata account on the same day; the
test calls the previous build's own instructions through it.

Replace a program's files with its deployed build whenever a release changes
one of its layouts, and the migrations and the upgrade test with them.
