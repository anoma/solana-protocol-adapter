//! What the harness sends a live cluster: settlements and the payer's own
//! setup, never an instruction of a deployment's owner or upgrade authority.
//! Every instruction is checked against an allowlist by program before its
//! transaction is sent; anything not on it is refused.

use std::collections::HashMap;

use anchor_lang::Discriminator;
use anoma_pa_solana_client::generated::protocol_adapter::client::args;
use anyhow::Context;
use solana_instruction::Instruction;
use solana_loader_v3_interface::instruction::UpgradeableLoaderInstruction;
use surfpool_sdk::Pubkey;

/// The adapter's instructions a live cluster is sent, by their IDL names: a
/// settlement's, its upload's, its simulation's, and the reads. Every other
/// instruction of the adapter is refused.
const ADAPTER_INSTRUCTIONS: [(&str, &[u8]); 10] = [
    ("settle", args::Settle::DISCRIMINATOR),
    ("settle_from_txdata", args::SettleFromTxdata::DISCRIMINATOR),
    ("simulate_execute", args::SimulateExecute::DISCRIMINATOR),
    ("txdata_init", args::TxdataInit::DISCRIMINATOR),
    ("txdata_write", args::TxdataWrite::DISCRIMINATOR),
    ("txdata_extend", args::TxdataExtend::DISCRIMINATOR),
    ("txdata_close", args::TxdataClose::DISCRIMINATOR),
    (
        "txdata_close_expired",
        args::TxdataCloseExpired::DISCRIMINATOR,
    ),
    (
        "risc_zero_verifier_paused",
        args::RiscZeroVerifierPaused::DISCRIMINATOR,
    ),
    ("version", args::Version::DISCRIMINATOR),
];

/// The adapter's IDL as the repository commits it, which names a refused
/// instruction of the adapter.
const ADAPTER_IDL: &str =
    include_str!("../../../../../solana-pa-prototype/idl/protocol_adapter.json");

/// The programs a live cluster is sent any instruction of: the payer's own
/// accounts and tokens, and the instructions a settlement carries before the
/// adapter's (its compute budget, a forwarder's signature check).
fn open_programs() -> [Pubkey; 6] {
    [
        solana_sdk_ids::system_program::ID,
        solana_sdk_ids::compute_budget::ID,
        solana_sdk_ids::ed25519_program::ID,
        spl_token_interface::ID,
        spl_token_2022_interface::ID,
        spl_associated_token_account_interface::program::ID,
    ]
}

/// The allowlist of a live cluster whose adapter is `adapter`.
pub(in crate::envs) struct LiveCluster {
    adapter: Pubkey,
    /// The instructions, by discriminator, a consumer registered for its
    /// programs.
    registered: HashMap<Pubkey, Vec<Vec<u8>>>,
}

impl LiveCluster {
    pub(in crate::envs) fn new(adapter: Pubkey) -> Self {
        Self {
            adapter,
            registered: HashMap::new(),
        }
    }

    /// Allows the instructions of `program` whose data starts with
    /// `discriminator`, which must not be empty: every instruction starts
    /// with no bytes. The loader's and the adapter's instructions are judged
    /// by their own rules, so registering them is an error.
    pub(in crate::envs) fn register(
        &mut self,
        program: Pubkey,
        discriminator: &[u8],
    ) -> anyhow::Result<()> {
        ensure_registrable(&self.adapter, &program, discriminator)?;
        self.registered
            .entry(program)
            .or_default()
            .push(discriminator.to_vec());
        Ok(())
    }

    /// Refuses `instructions` when one is not on the allowlist, naming its
    /// program and instruction.
    pub(in crate::envs) fn check(&self, instructions: &[Instruction]) -> anyhow::Result<()> {
        for ix in instructions {
            if let Some(refused) = self.refusal(ix)? {
                anyhow::bail!("refused on a live cluster: {refused}");
            }
        }
        Ok(())
    }

    /// `<program>/<instruction>` when `ix` is not on the allowlist.
    fn refusal(&self, ix: &Instruction) -> anyhow::Result<Option<String>> {
        let program = ix.program_id;
        let data = ix.data.as_slice();
        if open_programs().contains(&program) {
            return Ok(None);
        }
        if program == solana_sdk_ids::bpf_loader_upgradeable::ID {
            return Ok(Some(format!(
                "bpf_loader_upgradeable/{}",
                loader_instruction(data)
            )));
        }
        if program == self.adapter {
            if ADAPTER_INSTRUCTIONS
                .iter()
                .any(|(_, discriminator)| data.starts_with(discriminator))
            {
                return Ok(None);
            }
            return Ok(Some(format!(
                "protocol_adapter/{}",
                adapter_instruction(data)?
            )));
        }
        let registered = self.registered.get(&program).is_some_and(|discriminators| {
            discriminators
                .iter()
                .any(|discriminator| data.starts_with(discriminator))
        });
        Ok((!registered).then(|| format!("{program}/{:02x?}", leading(data))))
    }
}

/// Refuses registering `discriminator` for `program` when it cannot be
/// allowed: an empty one, which every instruction of `program` would match,
/// or any for the upgradeable loader or `adapter`, whose allowlist is fixed.
pub(in crate::envs) fn ensure_registrable(
    adapter: &Pubkey,
    program: &Pubkey,
    discriminator: &[u8],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        *program != solana_sdk_ids::bpf_loader_upgradeable::ID,
        "the loader's instructions cannot be allowed on a live cluster: the allowlist is fixed"
    );
    anyhow::ensure!(
        program != adapter,
        "the adapter's instructions cannot be allowed on a live cluster: the allowlist is fixed"
    );
    anyhow::ensure!(
        !discriminator.is_empty(),
        "an empty discriminator registered for {program} would allow every instruction of it"
    );
    Ok(())
}

/// The first bytes of `data`, where a discriminator is.
fn leading(data: &[u8]) -> &[u8] {
    &data[..data.len().min(8)]
}

/// The name of the loader instruction `data` encodes.
fn loader_instruction(data: &[u8]) -> String {
    match bincode::deserialize::<UpgradeableLoaderInstruction>(data) {
        Ok(UpgradeableLoaderInstruction::InitializeBuffer) => "InitializeBuffer".into(),
        Ok(UpgradeableLoaderInstruction::Write { .. }) => "Write".into(),
        Ok(UpgradeableLoaderInstruction::DeployWithMaxDataLen { .. }) => {
            "DeployWithMaxDataLen".into()
        }
        Ok(UpgradeableLoaderInstruction::Upgrade) => "Upgrade".into(),
        Ok(UpgradeableLoaderInstruction::SetAuthority) => "SetAuthority".into(),
        Ok(UpgradeableLoaderInstruction::Close) => "Close".into(),
        Ok(UpgradeableLoaderInstruction::ExtendProgram { .. }) => "ExtendProgram".into(),
        Ok(UpgradeableLoaderInstruction::SetAuthorityChecked) => "SetAuthorityChecked".into(),
        Err(_) => format!("{:02x?}", leading(data)),
    }
}

/// The IDL name of the adapter instruction `data` starts with.
fn adapter_instruction(data: &[u8]) -> anyhow::Result<String> {
    Ok(adapter_idl_instructions()?
        .into_iter()
        .find(|(_, discriminator)| data.starts_with(discriminator))
        .map_or_else(|| format!("{:02x?}", leading(data)), |(name, _)| name))
}

/// Every instruction of the committed adapter IDL: its name and
/// discriminator.
fn adapter_idl_instructions() -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let idl: serde_json::Value =
        serde_json::from_str(ADAPTER_IDL).context("the adapter's IDL is not JSON")?;
    idl["instructions"]
        .as_array()
        .context("the adapter's IDL has no instructions")?
        .iter()
        .map(|ix| {
            let name = ix["name"]
                .as_str()
                .context("an IDL instruction has no name")?;
            let discriminator = serde_json::from_value(ix["discriminator"].clone())
                .with_context(|| format!("the IDL instruction {name} has no discriminator"))?;
            Ok((name.to_string(), discriminator))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use anoma_pa_solana_client::instance::ProtocolAdapter;
    use solana_rpc_client::nonblocking::rpc_client::RpcClient;

    const ADAPTER: Pubkey = Pubkey::new_from_array([1; 32]);
    const FORWARDER: Pubkey = Pubkey::new_from_array([3; 32]);
    const KEY: Pubkey = Pubkey::new_from_array([5; 32]);

    fn refused(cluster: &LiveCluster, ix: Instruction, expected: &str) {
        let error = format!("{:#}", cluster.check(&[ix]).expect_err(expected));
        println!("{error}");
        assert_eq!(error, format!("refused on a live cluster: {expected}"));
    }

    #[test]
    fn every_allowed_adapter_instruction_is_the_idls() {
        let idl = adapter_idl_instructions().unwrap();
        for (name, discriminator) in ADAPTER_INSTRUCTIONS {
            let found = idl.iter().find(|(idl_name, _)| idl_name == name);
            println!("{name}: {found:02x?}");
            assert_eq!(
                found.map(|(_, d)| d.as_slice()),
                Some(discriminator),
                "the allowlist's {name} is not the IDL's instruction of that name"
            );
        }
    }

    #[test]
    fn every_loader_instruction_is_refused() {
        let cluster = LiveCluster::new(ADAPTER);
        for (name, instruction) in [
            (
                "InitializeBuffer",
                UpgradeableLoaderInstruction::InitializeBuffer,
            ),
            (
                "Write",
                UpgradeableLoaderInstruction::Write {
                    offset: 0,
                    bytes: vec![1],
                },
            ),
            (
                "DeployWithMaxDataLen",
                UpgradeableLoaderInstruction::DeployWithMaxDataLen { max_data_len: 1 },
            ),
            ("Upgrade", UpgradeableLoaderInstruction::Upgrade),
            ("SetAuthority", UpgradeableLoaderInstruction::SetAuthority),
            ("Close", UpgradeableLoaderInstruction::Close),
            (
                "ExtendProgram",
                UpgradeableLoaderInstruction::ExtendProgram {
                    additional_bytes: 1,
                },
            ),
            (
                "SetAuthorityChecked",
                UpgradeableLoaderInstruction::SetAuthorityChecked,
            ),
        ] {
            let ix = Instruction::new_with_bincode(
                solana_sdk_ids::bpf_loader_upgradeable::ID,
                &instruction,
                vec![],
            );
            refused(&cluster, ix, &format!("bpf_loader_upgradeable/{name}"));
        }
    }

    #[test]
    fn the_adapters_authority_instructions_are_refused_by_their_idl_names() {
        let cluster = LiveCluster::new(ADAPTER);
        let rpc = RpcClient::new("http://127.0.0.1:1".to_string());
        let instance = ProtocolAdapter::at(&rpc, ADAPTER);
        for (name, ix) in [
            (
                "initialize",
                instance.initialize_ix(&KEY, &KEY, &KEY, [1; 4]),
            ),
            ("pause", instance.pause_ix(&KEY)),
            ("unpause", instance.unpause_ix(&KEY)),
            (
                "set_kind_tree_root",
                instance.set_kind_tree_root_ix(&KEY, [7; 32]),
            ),
            ("deny_logic_refs", instance.deny_logic_refs_ix(&KEY, &[])),
            (
                "transfer_ownership",
                instance.transfer_ownership_ix(&KEY, &KEY),
            ),
            ("renounce_ownership", instance.renounce_ownership_ix(&KEY)),
            (
                "update_expiry_config",
                instance.update_expiry_config_ix(&KEY, 1, 2),
            ),
            ("upgrade", instance.upgrade_ix(&KEY, &KEY, &KEY)),
        ] {
            refused(&cluster, ix, &format!("protocol_adapter/{name}"));
        }
        refused(
            &cluster,
            Instruction::new_with_bytes(ADAPTER, &[0; 8], vec![]),
            "protocol_adapter/[00, 00, 00, 00, 00, 00, 00, 00]",
        );
    }

    #[test]
    fn settlements_and_the_open_programs_are_sent() {
        let cluster = LiveCluster::new(ADAPTER);
        let rpc = RpcClient::new("http://127.0.0.1:1".to_string());
        let instance = ProtocolAdapter::at(&rpc, ADAPTER);
        let accounts = || anoma_pa_solana_client::SettlementAccounts {
            new_root_marker: None,
            verifier_router_program: KEY,
            router: KEY,
            verifier_entry: KEY,
            verifier_program: KEY,
            remaining_accounts: vec![],
        };
        let mut sent = vec![
            instance.settle_ix(&KEY, &[1], accounts()),
            instance.settle_from_txdata_ix(&KEY, 0, accounts()),
            instance.simulate_execute_ix(&KEY, &[1], true, accounts()),
            instance.txdata_init_ix(&KEY, 0, 1, 2),
            instance.txdata_write_ix(&KEY, 0, 0, &[1]),
            instance.txdata_extend_ix(&KEY, 0, 3),
            instance.txdata_close_ix(&KEY, &KEY, 0),
            instance.txdata_close_expired_ix(&KEY, &KEY, &KEY, 0),
            instance.risc_zero_verifier_paused_ix(&KEY, [1; 4]),
            instance.version_ix(),
            solana_system_interface::instruction::transfer(&KEY, &KEY, 1),
        ];
        sent.extend(
            open_programs()
                .into_iter()
                .map(|program| Instruction::new_with_bytes(program, &[9; 8], vec![])),
        );
        cluster
            .check(&sent)
            .expect("a live cluster is sent settlements and the open programs");
    }

    #[test]
    fn another_program_is_sent_only_its_registered_instructions() {
        let mut cluster = LiveCluster::new(ADAPTER);
        let init = Instruction::new_with_bytes(FORWARDER, &[4; 12], vec![]);
        let owners = Instruction::new_with_bytes(FORWARDER, &[6; 12], vec![]);
        refused(
            &cluster,
            init.clone(),
            &format!("{FORWARDER}/[04, 04, 04, 04, 04, 04, 04, 04]"),
        );
        cluster.register(FORWARDER, &[4; 8]).unwrap();
        cluster
            .check(&[init])
            .expect("a registered instruction is sent");
        refused(
            &cluster,
            owners,
            &format!("{FORWARDER}/[06, 06, 06, 06, 06, 06, 06, 06]"),
        );
    }

    #[test]
    fn an_empty_discriminator_is_refused() {
        let mut cluster = LiveCluster::new(ADAPTER);
        let error = cluster.register(FORWARDER, &[]).unwrap_err();
        println!("{error:#}");
        assert!(
            error.to_string().contains("would allow every instruction"),
            "{error:#}"
        );
        refused(
            &cluster,
            Instruction::new_with_bytes(FORWARDER, &[4; 8], vec![]),
            &format!("{FORWARDER}/[04, 04, 04, 04, 04, 04, 04, 04]"),
        );
    }

    #[test]
    fn registering_the_loader_or_the_adapter_is_an_error() {
        let mut cluster = LiveCluster::new(ADAPTER);
        let upgrade = Instruction::new_with_bincode(
            solana_sdk_ids::bpf_loader_upgradeable::ID,
            &UpgradeableLoaderInstruction::Upgrade,
            vec![],
        );
        let error = cluster
            .register(solana_sdk_ids::bpf_loader_upgradeable::ID, &upgrade.data)
            .unwrap_err();
        println!("{error:#}");
        assert_eq!(
            error.to_string(),
            "the loader's instructions cannot be allowed on a live cluster: the allowlist is fixed"
        );
        refused(&cluster, upgrade, "bpf_loader_upgradeable/Upgrade");
        let pause = Instruction::new_with_bytes(ADAPTER, args::Pause::DISCRIMINATOR, vec![]);
        let error = cluster
            .register(ADAPTER, args::Pause::DISCRIMINATOR)
            .unwrap_err();
        println!("{error:#}");
        assert_eq!(
            error.to_string(),
            "the adapter's instructions cannot be allowed on a live cluster: the allowlist is fixed"
        );
        refused(&cluster, pause, "protocol_adapter/pause");
    }
}
