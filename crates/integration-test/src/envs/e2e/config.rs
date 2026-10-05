use std::env;

use anyhow::Context;

/// Configuration for the e2e environment, read from the process environment.
///
/// The runtime forks devnet, where the adapter is deployed at its
/// `env/devnet.env` address, through the operator's RPC endpoint; the proofs
/// come from the proving queue.
pub struct E2eConfig {
    /// The devnet RPC endpoint the runtime forks (the operator's provider,
    /// never the public endpoint).
    pub devnet_rpc_url: String,
    /// Base URL of the remote proving queue.
    pub queue_base_url: String,
    /// Auth token for the remote proving queue.
    pub queue_auth_token: String,
}

impl E2eConfig {
    /// Read the config from the environment (and a `.env` file, if any):
    /// `DEVNET_RPC_URL`, `QUEUE_BASE_URL` and `QUEUE_AUTH_TOKEN`.
    pub fn from_env() -> anyhow::Result<Self> {
        match dotenvy::dotenv() {
            Ok(_) => {}
            Err(e) if e.not_found() => {}
            Err(e) => return Err(e).context("failed to read the .env file"),
        }
        Ok(Self {
            devnet_rpc_url: env::var("DEVNET_RPC_URL").context("failed to read DEVNET_RPC_URL")?,
            queue_base_url: env::var("QUEUE_BASE_URL").context("failed to read QUEUE_BASE_URL")?,
            queue_auth_token: env::var("QUEUE_AUTH_TOKEN")
                .context("failed to read QUEUE_AUTH_TOKEN")?,
        })
    }
}
