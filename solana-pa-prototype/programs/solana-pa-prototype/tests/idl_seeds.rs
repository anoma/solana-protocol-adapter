//! Anchor's TypeScript account resolver fills a PDA account from an `arg`
//! seed by matching the seed's path against the instruction's argument names,
//! so every such path must name one of them.

use anchor_lang::idl::types::{Idl, IdlInstructionAccountItem, IdlSeed};

fn committed_idl() -> Idl {
    serde_json::from_str(include_str!("../../../idl/protocol_adapter.json"))
        .expect("idl/protocol_adapter.json is not an Anchor IDL")
}

fn arg_seed_paths(items: &[IdlInstructionAccountItem], out: &mut Vec<(String, String)>) {
    for item in items {
        match item {
            IdlInstructionAccountItem::Composite(group) => arg_seed_paths(&group.accounts, out),
            IdlInstructionAccountItem::Single(account) => {
                let Some(pda) = &account.pda else { continue };
                for seed in pda.seeds.iter().chain(pda.program.iter()) {
                    if let IdlSeed::Arg(arg) = seed {
                        out.push((account.name.clone(), arg.path.clone()));
                    }
                }
            }
        }
    }
}

#[test]
fn every_arg_seed_names_an_instruction_arg() {
    let idl = committed_idl();
    assert!(
        !idl.instructions.is_empty(),
        "the committed IDL declares no instructions"
    );
    let mut checked = 0;
    let mut failures = Vec::new();
    for instruction in &idl.instructions {
        let arg_names: Vec<&str> = instruction.args.iter().map(|a| a.name.as_str()).collect();
        let mut seeds = Vec::new();
        arg_seed_paths(&instruction.accounts, &mut seeds);
        for (account, path) in seeds {
            let first = path.split('.').next().expect("split yields a segment");
            println!(
                "{}: account {account}, arg seed {path}, args {arg_names:?}",
                instruction.name
            );
            checked += 1;
            if !arg_names.contains(&first) {
                failures.push(format!(
                    "instruction {}: account {account} has arg seed path {path}, which names none of its args {arg_names:?}",
                    instruction.name
                ));
            }
        }
    }
    assert!(checked > 0, "the committed IDL has no arg seeds to check");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
