use std::sync::Arc;

use anoma_pa_testkit::environment::{Environment, State, StateBuilder};
use anyhow::Context;
use solana_keypair::Keypair;

pub const KEY_DEFAULT_SIGNER: &str = "solana.actor.default_signer";

/// Records the funded keypair that pays for and signs the environment's
/// transactions.
#[inline]
pub fn insert_default_signer(builder: &mut StateBuilder, signer: Arc<Keypair>) {
    builder.insert(KEY_DEFAULT_SIGNER, signer);
}

#[inline]
pub fn default_signer<E: Environment>(env: &E) -> anyhow::Result<Arc<Keypair>> {
    default_signer_in_state(env.state())
}

#[inline]
pub fn default_signer_in_state(state: &State) -> anyhow::Result<Arc<Keypair>> {
    state
        .get::<Arc<Keypair>>(KEY_DEFAULT_SIGNER)
        .cloned()
        .context("failed to retrieve default signer from env")
}

#[cfg(test)]
mod tests {
    use super::*;
    use solana_signer::Signer;

    #[test]
    fn insert_and_resolve_default_signer() {
        let signer = Arc::new(Keypair::new());
        let state = {
            let mut builder = StateBuilder::new();
            insert_default_signer(&mut builder, signer.clone());
            builder.finalize()
        };
        assert_eq!(
            default_signer_in_state(&state).unwrap().pubkey(),
            signer.pubkey()
        );
        let err = default_signer_in_state(&StateBuilder::new().finalize()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "failed to retrieve default signer from env"
        );
    }
}
