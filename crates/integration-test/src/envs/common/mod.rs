//! Building blocks shared by the `local` and `e2e` environments: the runtime
//! and its transactions, the program addresses, and the protocol adapter with
//! its commitment tree.

pub(in crate::envs) mod addresses;
pub(in crate::envs) mod protocol_adapter;
pub(in crate::envs) mod runtime;
