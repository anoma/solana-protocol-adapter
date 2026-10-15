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
    fn the_localnet_file_names_the_adapter_and_the_mock_verifier() {
        assert_eq!(
            program_id(LOCALNET, "PROTOCOL_ADAPTER")
                .unwrap()
                .to_string(),
            "5zeqkB3kc9fd1RvaXB2GeMB53Jgf98QJtaFK38e6tTsc"
        );
        assert_eq!(
            program_id(LOCALNET, "MOCK_VERIFIER").unwrap().to_string(),
            "H3ZFoDHFvthGZu3kxpif3oSWm8MQn8uKvgDhrvVVHvHf"
        );
        assert_eq!(
            program_id(LOCALNET, "NO_SUCH").unwrap_err().to_string(),
            "the env file names no NO_SUCH_PROGRAM_ID="
        );
    }
}
