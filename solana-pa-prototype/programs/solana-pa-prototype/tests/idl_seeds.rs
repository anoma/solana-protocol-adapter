//! Every `arg` seed in the committed IDL names one of its instruction's
//! arguments, so Anchor's TypeScript account resolver can fill its account.

use anchor_lang::idl::types::Idl;
use protocol_adapter::idl_seeds::arg_seeds;

#[test]
fn every_arg_seed_names_an_instruction_arg() {
    let idl: Idl = serde_json::from_str(include_str!("../../../idl/protocol_adapter.json"))
        .expect("idl/protocol_adapter.json is not an Anchor IDL");
    let seeds = arg_seeds(&idl);
    for seed in &seeds {
        println!("{seed:?}");
    }
    assert!(
        !seeds.is_empty(),
        "the committed IDL has no arg seeds to check"
    );
    let failures: Vec<_> = seeds.iter().filter(|seed| !seed.names_an_arg()).collect();
    assert!(
        failures.is_empty(),
        "arg seeds naming no instruction arg: {failures:#?}"
    );
}
