//! Building blocks shared by the `local` and `e2e` environments: the
//! environment itself, the runtime and its transactions, the program
//! addresses, and the protocol adapter.

pub(in crate::envs) mod addresses;
pub mod environment;
pub mod protocol_adapter;
pub(in crate::envs) mod runtime;
