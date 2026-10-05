use anoma_pa_testkit::environment::{Environment, State, StateBuilder};
use anyhow::Context;
use surfpool_sdk::Pubkey;

pub const KEY_PA_PROGRAM: &str = "solana.pa.program";

/// Records the protocol adapter's program address.
#[inline]
pub fn insert_pa_program(builder: &mut StateBuilder, program: Pubkey) {
    builder.insert(KEY_PA_PROGRAM, program);
}

#[inline]
pub fn pa_program<E: Environment>(env: &E) -> anyhow::Result<Pubkey> {
    pa_program_in_state(env.state())
}

#[inline]
pub fn pa_program_in_state(state: &State) -> anyhow::Result<Pubkey> {
    state
        .get::<Pubkey>(KEY_PA_PROGRAM)
        .copied()
        .context("failed to retrieve protocol adapter program from env")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_resolve_pa_program() {
        let program = Pubkey::new_unique();
        let state = {
            let mut builder = StateBuilder::new();
            insert_pa_program(&mut builder, program);
            builder.finalize()
        };
        assert_eq!(pa_program_in_state(&state).unwrap(), program);
        assert_eq!(
            pa_program_in_state(&StateBuilder::new().finalize())
                .unwrap_err()
                .to_string(),
            "failed to retrieve protocol adapter program from env"
        );
    }
}
