use std::env::{self, VarError};
use std::path::PathBuf;

use anyhow::Context;

/// Configuration for the e2e environment, read from the process environment:
/// where it runs, and where its proofs come from (the proving queue or risc0
/// on this machine).
pub struct E2eConfig {
    pub runtime: E2eRuntime,
    /// The remote proving queue to prove with; `None` proves with risc0 on
    /// this machine.
    pub queue: Option<QueueConfig>,
}

/// Where the e2e environment runs.
#[derive(Debug, PartialEq, Eq)]
pub enum E2eRuntime {
    /// A runtime of its own, as the local environment's, with the adapter
    /// deployed and initialized against devnet's Groth16 verifier, from the
    /// copy the adapter repository commits.
    Local,
    /// A fork of devnet, through the operator's RPC endpoint (never the
    /// public one): the adapter devnet runs, at its `env/devnet.env`
    /// address, with the state devnet holds.
    Devnet { rpc_url: String },
    /// The cluster itself, through the operator's RPC endpoint (never the
    /// public one): transactions are sent to it, signed and paid for by the
    /// keypair at `payer`, against the adapter at its `env/devnet.env`
    /// address, with the state the cluster holds.
    Cluster { rpc_url: String, payer: PathBuf },
}

/// The remote proving queue's endpoint and credentials.
pub struct QueueConfig {
    pub base_url: String,
    pub auth_token: String,
}

/// The hosts of the clusters' public RPC endpoints, which rate-limit the
/// tests' traffic: the tests reach a cluster through the operator's endpoint.
const PUBLIC_RPC_HOSTS: [&str; 2] = ["api.devnet.solana.com", "api.mainnet-beta.solana.com"];

/// The payer of the cluster runtime when `E2E_PAYER` is unset: the operator's
/// devnet wallet, where the adapter repository's scripts keep it.
const DEFAULT_CLUSTER_PAYER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../solana-pa-prototype/scripts/devnet-wallet.json"
);

/// `url`, the endpoint the variable `name` gives, unless it is a cluster's
/// public endpoint.
fn operator_rpc_url(name: &str, url: String) -> anyhow::Result<String> {
    let parsed = url::Url::parse(&url).with_context(|| format!("{name} {url:?} is not a URL"))?;
    let host = parsed
        .host_str()
        .with_context(|| format!("{name} {url:?} names no host"))?;
    anyhow::ensure!(
        !PUBLIC_RPC_HOSTS.contains(&host),
        "{name} is {host}: the public RPC is not used; set it to the operator's endpoint"
    );
    Ok(url)
}

impl E2eConfig {
    /// Read the config from the environment (and a `.env` file, if any): see
    /// `from_vars`.
    pub fn from_env() -> anyhow::Result<Self> {
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => return Err(e).context("failed to read the .env file"),
        }
        Self::from_vars(|name| env::var(name))
    }

    /// Read the config from the variables `var` gives: `E2E_RUNTIME`
    /// (`local`, the default; `devnet`, with `DEVNET_RPC_URL`; or `cluster`,
    /// with `E2E_RPC_URL` and `E2E_PAYER`, by default the operator's devnet
    /// wallet), and `E2E_PROVER` (`queue` or `local`; by default `queue`
    /// when `QUEUE_BASE_URL` is set, `local` otherwise), with
    /// `QUEUE_BASE_URL` and `QUEUE_AUTH_TOKEN` for the queue.
    fn from_vars(var: impl Fn(&str) -> Result<String, VarError>) -> anyhow::Result<Self> {
        let read = |name: &str| var(name).with_context(|| format!("failed to read {name}"));
        let runtime = match var("E2E_RUNTIME") {
            Ok(runtime) => runtime,
            Err(VarError::NotPresent) => "local".to_string(),
            Err(e) => return Err(e).context("failed to read E2E_RUNTIME"),
        };
        let runtime = match runtime.as_str() {
            "local" => E2eRuntime::Local,
            "devnet" => E2eRuntime::Devnet {
                rpc_url: operator_rpc_url("DEVNET_RPC_URL", read("DEVNET_RPC_URL")?)?,
            },
            "cluster" => E2eRuntime::Cluster {
                rpc_url: operator_rpc_url("E2E_RPC_URL", read("E2E_RPC_URL")?)?,
                payer: match var("E2E_PAYER") {
                    Ok(payer) => PathBuf::from(payer),
                    Err(VarError::NotPresent) => PathBuf::from(DEFAULT_CLUSTER_PAYER),
                    Err(e) => return Err(e).context("failed to read E2E_PAYER"),
                },
            },
            other => anyhow::bail!("E2E_RUNTIME is {other:?}, not local, devnet or cluster"),
        };
        let prover = match var("E2E_PROVER") {
            Ok(prover) => prover,
            Err(VarError::NotPresent)
                if !matches!(var("QUEUE_BASE_URL"), Err(VarError::NotPresent)) =>
            {
                "queue".to_string()
            }
            Err(VarError::NotPresent) => "local".to_string(),
            Err(e) => return Err(e).context("failed to read E2E_PROVER"),
        };
        let queue = match prover.as_str() {
            "queue" => Some(QueueConfig {
                base_url: read("QUEUE_BASE_URL")?,
                auth_token: read("QUEUE_AUTH_TOKEN")?,
            }),
            "local" => None,
            other => anyhow::bail!("E2E_PROVER is {other:?}, not queue or local"),
        };
        Ok(Self { runtime, queue })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The config the variables `vars` give, and nothing else.
    fn config(vars: &[(&str, &str)]) -> anyhow::Result<E2eConfig> {
        let vars: HashMap<&str, &str> = vars.iter().copied().collect();
        E2eConfig::from_vars(|name| {
            vars.get(name)
                .map(|value| value.to_string())
                .ok_or(VarError::NotPresent)
        })
    }

    const HELIUS: &str = "https://devnet.helius-rpc.com/?api-key=k";

    #[test]
    fn cluster_runtime_reads_its_url_and_payer() {
        let runtime = config(&[
            ("E2E_RUNTIME", "cluster"),
            ("E2E_RPC_URL", HELIUS),
            ("E2E_PAYER", "/keys/payer.json"),
        ])
        .unwrap()
        .runtime;
        println!("{runtime:?}");
        assert_eq!(
            runtime,
            E2eRuntime::Cluster {
                rpc_url: HELIUS.to_string(),
                payer: PathBuf::from("/keys/payer.json"),
            }
        );

        let runtime = config(&[("E2E_RUNTIME", "cluster"), ("E2E_RPC_URL", HELIUS)])
            .unwrap()
            .runtime;
        println!("without E2E_PAYER: {runtime:?}");
        let E2eRuntime::Cluster { payer, .. } = runtime else {
            panic!("E2E_RUNTIME=cluster read as {runtime:?}");
        };
        assert!(
            payer.ends_with("solana-pa-prototype/scripts/devnet-wallet.json"),
            "the default payer is {}, not the operator's devnet wallet",
            payer.display()
        );

        let missing = config(&[("E2E_RUNTIME", "cluster")]).err().unwrap();
        println!("without E2E_RPC_URL: {missing:#}");
        assert!(
            format!("{missing:#}").contains("failed to read E2E_RPC_URL"),
            "{missing:#}"
        );
    }

    #[test]
    fn cluster_runtime_refuses_the_public_rpc() {
        for url in [
            "https://api.devnet.solana.com",
            "https://api.mainnet-beta.solana.com/",
            "http://user@api.devnet.solana.com:8899/path",
        ] {
            let refused = config(&[("E2E_RUNTIME", "cluster"), ("E2E_RPC_URL", url)])
                .err()
                .unwrap_or_else(|| panic!("the cluster runtime accepts {url}"));
            println!("{url}: {refused:#}");
            assert!(
                format!("{refused:#}").contains("the public RPC is not used"),
                "{url}: {refused:#}"
            );
        }
    }

    #[test]
    fn the_devnet_fork_refuses_the_public_rpc() {
        let refused = config(&[
            ("E2E_RUNTIME", "devnet"),
            ("DEVNET_RPC_URL", "https://api.devnet.solana.com"),
        ])
        .err()
        .unwrap();
        println!("{refused:#}");
        assert!(
            format!("{refused:#}").contains("the public RPC is not used"),
            "{refused:#}"
        );
    }

    #[test]
    fn an_unknown_runtime_is_refused() {
        let refused = config(&[("E2E_RUNTIME", "testnet")]).err().unwrap();
        assert_eq!(
            refused.to_string(),
            "E2E_RUNTIME is \"testnet\", not local, devnet or cluster"
        );
    }
}
