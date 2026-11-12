//! The program addresses the repository records per cluster
//! (`solana-pa-prototype/env/<cluster>.env`), read as the scripts read them.

use anyhow::Context;
use surfpool_sdk::Pubkey;

pub(in crate::envs) const LOCALNET: &str =
    include_str!("../../../../../solana-pa-prototype/env/localnet.env");
#[cfg(feature = "e2e")]
pub(in crate::envs) const DEVNET: &str =
    include_str!("../../../../../solana-pa-prototype/env/devnet.env");

/// The address `<NAME>_PROGRAM_ID` names in an env file.
pub(in crate::envs) fn program_id(env_file: &str, name: &str) -> anyhow::Result<Pubkey> {
    let key = format!("{name}_PROGRAM_ID=");
    let value = env_file
        .lines()
        .find_map(|line| line.strip_prefix(&key))
        .with_context(|| format!("the env file names no {key}"))?;
    value
        .parse()
        .with_context(|| format!("{key}{value} is not a base58 address"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_reads_its_own_line() {
        let (first, second) = (Pubkey::new_unique(), Pubkey::new_unique());
        let env_file = format!("# comment\nFIRST_PROGRAM_ID={first}\nSECOND_PROGRAM_ID={second}\n");
        assert_eq!(program_id(&env_file, "FIRST").unwrap(), first);
        assert_eq!(program_id(&env_file, "SECOND").unwrap(), second);
        program_id(&env_file, "THIRD").unwrap_err();
        program_id("BAD_PROGRAM_ID=not-base58\n", "BAD").unwrap_err();
    }

    #[test]
    fn the_localnet_file_names_the_adapter_and_the_mock_verifier() {
        program_id(LOCALNET, "PROTOCOL_ADAPTER").unwrap();
        program_id(LOCALNET, "MOCK_VERIFIER").unwrap();
    }
}
