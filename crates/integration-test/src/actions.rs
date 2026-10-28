//! Actions whose resources come in any order, consumed and created
//! interleaved, each under the pass-through logic with the app data it is
//! given, or under the trivial logic. pa-testkit's builders make an action's
//! consumed resources first, then its created ones.

use anoma_pa_testkit::fixtures::kind_tree;
use anoma_pa_testkit::fixtures::passthrough::{PASSTHROUGH_LOGIC_PK, PASSTHROUGH_LOGIC_VK};
use anoma_pa_testkit::witness::{ActionWitnesses, AppData, LogicWitness};
use anoma_rm_risc0::Digest;
use anoma_rm_risc0::action_tree::ActionTree;
use anoma_rm_risc0::compliance;
use anoma_rm_risc0::constants::PADDING_LOGIC_VK;
use anoma_rm_risc0::logic_instance::LogicInstance;
use anoma_rm_risc0::nullifier_key::NullifierKey;
use anoma_rm_risc0::resource::{Resource, ResourceWitness, ResourceWitnessConsumed};
use anoma_rm_risc0::resource_logic::TrivialLogicWitness;
use anyhow::Context;

/// The logic of one resource of an action.
pub enum Logic {
    /// The pass-through logic, committing this app data.
    Passthrough(AppData),
    /// The trivial logic, which commits no app data.
    Trivial,
}

/// One resource of an action: whether it is consumed, and its logic.
pub struct ResourceSpec {
    pub consumed: bool,
    pub logic: Logic,
}

/// Builds the action whose resources are `resources`, in that order: ephemeral
/// and of zero quantity, so the action balances, with nonces from `seed`.
/// Its consumed resources name the commitment tree root `ephemeral_root`.
pub fn build(
    seed: u8,
    resources: Vec<ResourceSpec>,
    ephemeral_root: Digest,
) -> anyhow::Result<ActionWitnesses> {
    let nf_key = NullifierKey::from_bytes([seed; 32]);
    let resource_at = |index: usize, logic: &Logic| Resource {
        logic_ref: match logic {
            Logic::Passthrough(_) => PASSTHROUGH_LOGIC_VK,
            Logic::Trivial => PADDING_LOGIC_VK,
        },
        quantity: 0,
        is_ephemeral: true,
        nonce: nonce(seed, index),
        nk_commitment: nf_key.commit(),
        rand_seed: [seed.wrapping_add(index as u8); 32],
        ..Resource::default()
    };

    let nullifier = |resource: &Resource| {
        resource
            .nullifier(&nf_key)
            .context("failed to compute a consumed nullifier")
    };
    let nullifiers: Vec<Digest> = resources
        .iter()
        .enumerate()
        .filter(|(_, spec)| spec.consumed)
        .map(|(index, spec)| nullifier(&resource_at(index, &spec.logic)))
        .collect::<anyhow::Result<_>>()?;

    let mut witnesses = Vec::with_capacity(resources.len());
    let mut tagged = Vec::with_capacity(resources.len());
    let mut created_index = 0;
    for (index, spec) in resources.into_iter().enumerate() {
        let mut resource = resource_at(index, &spec.logic);
        if spec.consumed {
            let tag = nullifier(&resource)?;
            witnesses.push(ResourceWitness::Consumed(
                ResourceWitnessConsumed::from_resource(resource, nf_key.clone()),
            ));
            tagged.push((resource, tag, spec));
        } else {
            // A created resource's nonce derives from its index among the
            // created resources and the action's nullifiers; the compliance
            // circuit rejects any other.
            resource.nonce = Resource::derive_nonce_from_nullifiers(created_index, &nullifiers)
                .context("failed to derive a created nonce")?;
            created_index += 1;
            witnesses.push(ResourceWitness::Created(resource));
            tagged.push((resource, resource.commitment(), spec));
        }
    }

    let compliance_witness =
        compliance::from_resources_with_ephemeral_root(witnesses, ephemeral_root, kind_tree());
    let action_tree_root = ActionTree::new(tagged.iter().map(|(_, tag, _)| *tag).collect())
        .root()
        .context("failed to compute the action tree root")?;
    let logic_witnesses = tagged
        .into_iter()
        .map(|(resource, tag, spec)| -> Box<dyn LogicWitness> {
            match spec.logic {
                Logic::Passthrough(app_data) => Box::new(Passthrough(LogicInstance {
                    tag,
                    is_consumed: spec.consumed,
                    action_tree_root,
                    app_data,
                })),
                Logic::Trivial => Box::new(TrivialLogicWitness {
                    resource,
                    action_tree_root,
                    is_consumed: spec.consumed,
                    nf_key: nf_key.clone(),
                }),
            }
        })
        .collect();
    Ok(ActionWitnesses {
        compliance_witness: Box::new(compliance_witness),
        logic_witnesses,
    })
}

/// The nonce of the resource at `index` of the action built from `seed`.
fn nonce(seed: u8, index: usize) -> [u8; 32] {
    let mut nonce = *b"solana-pa harness list order\0\0\0\0";
    nonce[30] = seed;
    nonce[31] = index as u8;
    nonce
}

/// The pass-through logic's witness: the logic instance its guest reads and
/// commits unchanged.
struct Passthrough(LogicInstance);

impl LogicWitness for Passthrough {
    fn verifying_key(&self) -> Digest {
        PASSTHROUGH_LOGIC_VK
    }

    fn constrain(&self) -> anyhow::Result<LogicInstance> {
        Ok(self.0.clone())
    }

    fn witness_to_vec(&self) -> anyhow::Result<Vec<u32>> {
        let journal = anoma_rm_risc0::proving_system::instance_to_journal(&self.0)
            .map_err(|error| anyhow::anyhow!("failed to serialize the witness: {error:?}"))?;
        Ok(anoma_rm_risc0::utils::bytes_to_words(&journal))
    }

    fn proving_key(&self) -> Vec<u8> {
        PASSTHROUGH_LOGIC_PK.to_vec()
    }
}
