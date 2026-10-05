use anoma_pa_testkit::environment::{Environment, State, StateBuilder};
use anyhow::Context;

pub const KEY_CLUSTER: &str = "solana.cluster";
pub const KEY_RPC_URL: &str = "solana.cluster.rpc_url";

/// The cluster an environment's runtime holds: a fresh local one, or a fork
/// of devnet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cluster {
    Localnet,
    Devnet,
}

/// Records the cluster the runtime holds and the runtime's RPC endpoint.
#[inline]
pub fn insert_cluster(builder: &mut StateBuilder, cluster: Cluster, rpc_url: String) {
    builder.insert(KEY_CLUSTER, cluster);
    builder.insert(KEY_RPC_URL, rpc_url);
}

#[inline]
pub fn cluster<E: Environment>(env: &E) -> anyhow::Result<Cluster> {
    cluster_in_state(env.state())
}

#[inline]
pub fn cluster_in_state(state: &State) -> anyhow::Result<Cluster> {
    state
        .get::<Cluster>(KEY_CLUSTER)
        .copied()
        .context("failed to retrieve the cluster from env")
}

#[inline]
pub fn rpc_url<E: Environment>(env: &E) -> anyhow::Result<String> {
    rpc_url_in_state(env.state())
}

#[inline]
pub fn rpc_url_in_state(state: &State) -> anyhow::Result<String> {
    state
        .get::<String>(KEY_RPC_URL)
        .cloned()
        .context("failed to retrieve the RPC endpoint from env")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_resolve_cluster_and_rpc_url() {
        let state = {
            let mut builder = StateBuilder::new();
            insert_cluster(
                &mut builder,
                Cluster::Devnet,
                "http://127.0.0.1:1".to_string(),
            );
            builder.finalize()
        };
        assert_eq!(cluster_in_state(&state).unwrap(), Cluster::Devnet);
        assert_eq!(rpc_url_in_state(&state).unwrap(), "http://127.0.0.1:1");
        let empty = StateBuilder::new().finalize();
        assert_eq!(
            cluster_in_state(&empty).unwrap_err().to_string(),
            "failed to retrieve the cluster from env"
        );
    }
}
