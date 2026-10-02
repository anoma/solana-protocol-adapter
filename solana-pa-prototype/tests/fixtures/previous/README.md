# Previous builds

The builds deployed on devnet before the in-place upgrade to this build, which
`tests/upgrade/` starts from. Each spec file `tests/upgrade/<program>.ts` runs
on its own validator, which loads `<program>.so` from here at that program's
id instead of this build (scripts/anchor-test.sh), upgrades the program in
place and migrates its accounts.

`protocol_adapter.so` is the adapter as deployed on devnet at
`5zeqkB3kc9fd1RvaXB2GeMB53Jgf98QJtaFK38e6tTsc` (state schema version 2, owned
by the program's upgrade authority), dumped with
`solana program dump 5zeqkB3kc9fd1RvaXB2GeMB53Jgf98QJtaFK38e6tTsc` on
2026-10-02: the deterministic build of `anthony/arm-v2-port` `2da4151`,
executable hash `100d6e51412f6fa072e291eab6070ab7dfcf039367d543e1c1bc450af04078f2`
(see docs/DEVNET_DEPLOYMENT.md). `protocol_adapter.json` is its production
IDL, fetched with `anchor idl fetch` from its Program Metadata account on the
same day; the test calls the previous build through it.

`spl_token_forwarder.so` is the SPL token forwarder as deployed on devnet at
`BsfuXpxw8oCmZXnYijyQkUYNcCnuskFZbYizmWLnpSU7` (its config without an owner,
owned by the program's upgrade authority), dumped with
`solana program dump BsfuXpxw8oCmZXnYijyQkUYNcCnuskFZbYizmWLnpSU7` on
2026-10-02: the production build of `anthony/forwarder-cpi-events` `b726c0e`,
executable hash `155b957b60f86c8511b3515487f2e07c25ed49e17b14d81432b1689997a80004`
(see docs/DEVNET_DEPLOYMENT.md). The forwarder has no on-chain IDL;
`spl_token_forwarder.json` is that code's production IDL as
anoma-pa-solana-client records it (`idl/spl_token_forwarder.json` at
`9c2111e`, from this repository's production build at `863e095`, whose
program sources are `b726c0e`'s).

Replace a program's files with its deployed build whenever a release changes
one of its layouts, and the migrations and the upgrade test with them.
