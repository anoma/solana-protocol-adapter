//! Actions the harness's tests share.

use anoma_pa_testkit::fixtures::passthrough;
use anoma_pa_testkit::witness::ActionWitnesses;
use anyhow::Context;

/// One pass-through action carrying a 4,800-byte application payload: a
/// settlement too large in bytes for one transaction, light in compute.
/// `seed` keeps its nonces apart from other actions'.
pub fn oversized_passthrough(seed: u8) -> anyhow::Result<ActionWitnesses> {
    Ok(passthrough::build(
        seed,
        vec![],
        passthrough::Overrides {
            application_payload: vec![vec![0x5a5a_5a5a; 1200]],
            ..passthrough::Overrides::default()
        },
    )
    .context("failed to build the pass-through action")?
    .witnesses)
}
