use std::env;

use anyhow::Context;

/// Configuration for the e2e environment, read from the process environment.
///
/// The runtime forks devnet, where the adapter is deployed at its
/// `env/devnet.env` address, through the operator's RPC endpoint; the proofs
/// come from the proving queue or from risc0 on this machine.
pub struct E2eConfig {
    /// The devnet RPC endpoint the runtime forks (the operator's provider,
    /// never the public endpoint).
    pub devnet_rpc_url: String,
    /// The remote proving queue to prove with; `None` proves with risc0 on
    /// this machine.
    pub queue: Option<QueueConfig>,
}

/// The remote proving queue's endpoint and credentials.
pub struct QueueConfig {
    pub base_url: String,
    pub auth_token: String,
}

impl E2eConfig {
    /// Read the config from the environment (and a `.env` file, if any):
    /// `DEVNET_RPC_URL`, and `E2E_PROVER` (`queue` or `local`; by default
    /// `queue` when `QUEUE_BASE_URL` is set, `local` otherwise), with
    /// `QUEUE_BASE_URL` and `QUEUE_AUTH_TOKEN` for the queue.
    pub fn from_env() -> anyhow::Result<Self> {
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => return Err(e).context("failed to read the .env file"),
        }
        let prover = match env::var("E2E_PROVER") {
            Ok(prover) => prover,
            Err(env::VarError::NotPresent) if env::var_os("QUEUE_BASE_URL").is_some() => {
                "queue".to_string()
            }
            Err(env::VarError::NotPresent) => "local".to_string(),
            Err(e) => return Err(e).context("failed to read E2E_PROVER"),
        };
        let queue = match prover.as_str() {
            "queue" => Some(QueueConfig {
                base_url: env::var("QUEUE_BASE_URL").context("failed to read QUEUE_BASE_URL")?,
                auth_token: env::var("QUEUE_AUTH_TOKEN")
                    .context("failed to read QUEUE_AUTH_TOKEN")?,
            }),
            "local" => None,
            other => anyhow::bail!("E2E_PROVER is {other:?}, not queue or local"),
        };
        Ok(Self {
            devnet_rpc_url: env::var("DEVNET_RPC_URL").context("failed to read DEVNET_RPC_URL")?,
            queue,
        })
    }
}
