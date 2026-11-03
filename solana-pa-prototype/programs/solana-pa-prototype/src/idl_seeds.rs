//! Anchor's TypeScript account resolver fills a PDA account from an `arg`
//! seed by matching the seed's path against the instruction's argument names,
//! so every such path must name one of them. Anchor takes the argument names
//! from the handler and the seed paths from `#[instruction(...)]`, so nothing
//! in the build checks that they agree.

use anchor_lang::idl::types::{Idl, IdlInstructionAccountItem, IdlSeed};

/// An `arg` seed of a PDA account in an instruction of an IDL.
#[derive(Debug)]
pub struct ArgSeed {
    pub instruction: String,
    pub account: String,
    pub path: String,
    /// The instruction's argument names.
    pub args: Vec<String>,
}

impl ArgSeed {
    /// Whether the seed's path starts with one of the instruction's arguments.
    pub fn names_an_arg(&self) -> bool {
        let first = self.path.split('.').next().expect("split yields a segment");
        self.args.iter().any(|arg| arg == first)
    }
}

/// Every `arg` seed of every PDA account of every instruction in `idl`.
pub fn arg_seeds(idl: &Idl) -> Vec<ArgSeed> {
    fn collect(
        instruction: &str,
        args: &[String],
        items: &[IdlInstructionAccountItem],
        out: &mut Vec<ArgSeed>,
    ) {
        for item in items {
            match item {
                IdlInstructionAccountItem::Composite(group) => {
                    collect(instruction, args, &group.accounts, out)
                }
                IdlInstructionAccountItem::Single(account) => {
                    let Some(pda) = &account.pda else { continue };
                    for seed in pda.seeds.iter().chain(pda.program.iter()) {
                        if let IdlSeed::Arg(arg) = seed {
                            out.push(ArgSeed {
                                instruction: instruction.to_owned(),
                                account: account.name.clone(),
                                path: arg.path.clone(),
                                args: args.to_vec(),
                            });
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for instruction in &idl.instructions {
        let args: Vec<String> = instruction.args.iter().map(|a| a.name.clone()).collect();
        collect(&instruction.name, &args, &instruction.accounts, &mut out);
    }
    out
}
