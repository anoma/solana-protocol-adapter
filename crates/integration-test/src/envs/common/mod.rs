//! Building blocks shared by the `local` and `e2e` environments: the
//! environment itself, the runtime and its transactions, the program
//! addresses, the adapter's deployment, the protocol adapter, and what a
//! live cluster is sent.

pub(in crate::envs) mod addresses;
mod block_time_forwarder;
pub(in crate::envs) mod deployment;
pub mod environment;
mod live_cluster;
pub mod protocol_adapter;
pub(in crate::envs) mod runtime;
