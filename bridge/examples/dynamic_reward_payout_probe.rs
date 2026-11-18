use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{
    config::{
        genesis::MiningReserveAllocation,
        mining_reserve,
        params::{OverrideParams, SIMNET_PARAMS},
    },
    constants::{MAX_TX_IN_SEQUENCE_NUM, TX_VERSION},
    mass::MassCalculator,
    network::NetworkType,
    sign,
    subnets::SUBNETWORK_ID_NATIVE,
    tx::{SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry},
};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{RpcTransaction, SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use celenium_stratum_bridge::{CeleniumApi, transfer::PreparedTransfer};
use celenium_txscript::pay_to_address_script;
use secp256k1::{Keypair, SECP256K1};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

const PAYOUT: u64 = 2_500_000;

struct Wallet {
    address: Address,
    secret: [u8; 32],
}

impl Wallet {
    fn new() -> Self {
        let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
        Self {
            address: Address::new(Prefix::Simnet, Version::PubKey, &pair.x_only_public_key().0.serialize()),
            secret: pair.secret_bytes(),
        }
    }

    async fn prepare(&self, api: &CeleniumApi, destination: &Address, amount: u64) -> Result<PreparedTransfer> {
        api.prepare_transfer(&self.address.to_string(), &destination.to_string(), amount, &hex::encode(self.secret)).await
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    balance: u64,
    outputs: Vec<(String, u32, u64, u64, bool)>,
}

async fn snapshot(client: &GrpcClient, address: &Address) -> Result<Snapshot> {
    let entries = client.get_utxos_by_addresses(vec![address.clone()]).await?;
    let mut outputs = entries
        .iter()
        .map(|entry| {
            (
                entry.outpoint.transaction_id.to_string(),
                entry.outpoint.index,
                entry.utxo_entry.amount,
                entry.utxo_entry.block_daa_score,
                entry.utxo_entry.is_coinbase,
            )
        })
        .collect::<Vec<_>>();
    outputs.sort();
    let balance = client.get_balance_by_address(address.clone()).await?;
    ensure!(entries.iter().map(|entry| entry.utxo_entry.amount).sum::<u64>() == balance, "UTXO index balance mismatch");
    Ok(Snapshot { balance, outputs })
}

async fn single_input(client: &GrpcClient, address: &Address) -> Result<(TransactionOutpoint, UtxoEntry)> {
    let mut entries = client.get_utxos_by_addresses(vec![address.clone()]).await?;
    ensure!(entries.len() == 1, "probe requires exactly one source UTXO");
    let entry = entries.pop().context("missing source UTXO")?;
    let utxo: UtxoEntry = entry.utxo_entry.into();
    ensure!(utxo.script_public_key == pay_to_address_script(address));
    let daa = client.get_block_dag_info().await?.virtual_daa_score;
    ensure!(!utxo.is_coinbase || utxo.block_daa_score + SIMNET_PARAMS.coinbase_maturity() <= daa, "source UTXO is immature");
    Ok((entry.outpoint.into(), utxo))
}

fn signed_transfer(
    input: &(TransactionOutpoint, UtxoEntry),
    source: &Wallet,
    destination: &Address,
    amount: u64,
    fee: u64,
) -> Result<SignableTransaction> {
    let change = input.1.amount.checked_sub(amount).and_then(|value| value.checked_sub(fee)).context("insufficient funds")?;
    let mut outputs = vec![TransactionOutput::new(amount, pay_to_address_script(destination))];
    if change > 0 {
        outputs.push(TransactionOutput::new(change, pay_to_address_script(&source.address)));
    }
    let tx = Transaction::new(
        TX_VERSION,
        vec![TransactionInput::new(input.0, vec![], MAX_TX_IN_SEQUENCE_NUM, 1)],
        outputs,
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    let signed =
        sign::sign_with_multiple_v2(SignableTransaction::with_entries(tx, vec![input.1.clone()]), &[source.secret]).fully_signed()?;
    sign::verify(&signed.as_verifiable())?;
    Ok(signed)
}

fn inspect_signed(label: &str, signed: &SignableTransaction) -> Result<u64> {
    sign::verify(&signed.as_verifiable())?;
    let calculator = MassCalculator::new_with_consensus_params(&SIMNET_PARAMS);
    let non = calculator.calc_non_contextual_masses(&signed.tx);
    let storage = calculator.calc_contextual_masses(&signed.as_verifiable()).context("incomputable storage mass")?.storage_mass;
    let total = signed.entries.iter().map(|entry| entry.as_ref().map_or(0, |entry| entry.amount)).sum::<u64>();
    let values = signed.tx.outputs.iter().map(|output| output.value).collect::<Vec<_>>();
    let fee = total.checked_sub(values.iter().sum()).context("outputs exceed inputs")?;
    println!(
        "MASS case={label} inputs={} input_total={total} outputs={values:?} compute={} transient={} storage={storage} storage_limit={} fee={fee} signatures_verified=true",
        signed.tx.inputs.len(),
        non.compute_mass,
        non.transient_mass,
        SIMNET_PARAMS.block_mass_limits.storage
    );
    ensure!(non.compute_mass <= SIMNET_PARAMS.block_mass_limits.compute);
    ensure!(non.transient_mass <= SIMNET_PARAMS.block_mass_limits.transient);
    Ok(storage)
}

fn unpack(prepared: &PreparedTransfer) -> Result<SignableTransaction> {
    let rpc: RpcTransaction = serde_json::from_str(&prepared.transaction_json()?)?;
    let version = rpc.version;
    let inputs = rpc
        .inputs
        .into_iter()
        .map(|input| {
            if version >= 1 {
                TransactionInput::new_with_compute_budget(
                    input.previous_outpoint.into(),
                    input.signature_script,
                    input.sequence,
                    input.compute_budget,
                )
            } else {
                TransactionInput::new(input.previous_outpoint.into(), input.signature_script, input.sequence, input.sig_op_count)
            }
        })
        .collect();
    let outputs = rpc
        .outputs
        .into_iter()
        .map(|output| {
            TransactionOutput::with_covenant(output.value, output.script_public_key, output.covenant.map(|binding| binding.0))
        })
        .collect();
    let tx = Transaction::new_with_mass(
        rpc.version,
        inputs,
        outputs,
        rpc.lock_time,
        rpc.subnetwork_id,
        rpc.gas,
        rpc.payload,
        rpc.storage_mass,
    );
    Ok(SignableTransaction::with_entries(tx, prepared.input_entries().into_iter().map(Into::into).collect()))
}

async fn mine(client: &GrpcClient, miner: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let before = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(miner.clone(), b"dynamic-reward-payout-proof".to_vec()).await?;
        ensure!(
            matches!(client.submit_block(template.block, false).await?.report, SubmitBlockReport::Success),
            "simnet block rejected"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= before {
            ensure!(Instant::now() < deadline, "virtual DAA stalled");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Ok(())
}

async fn reject(
    client: &GrpcClient,
    api: &CeleniumApi,
    miner: &Address,
    source: &Wallet,
    destination: &Address,
    amount: u64,
    label: &str,
) -> Result<()> {
    let before = snapshot(client, &source.address).await?;
    let destination_before = snapshot(client, destination).await?;
    ensure!(client.get_mempool_entries(true, false).await?.is_empty());
    let error = source.prepare(api, destination, amount).await.expect_err("bridge unexpectedly prepared an oversized transfer");
    ensure!(error.to_string().contains("SmallTransferNotSupported"), "unexpected bridge rejection: {error:#}");
    println!("BRIDGE_REJECTED case={label} amount={amount} reason={error:#}");

    // Direct RPC checks the node independently of the bridge's preflight rejection.
    let input = single_input(client, &source.address).await?;
    let signed = signed_transfer(&input, source, destination, amount, api.estimate_standard_fee().await?)?;
    let storage = inspect_signed(label, &signed)?;
    ensure!(storage > SIMNET_PARAMS.block_mass_limits.storage);
    let rpc: RpcTransaction = serde_json::from_str(&serde_json::to_string(&signed.tx)?)?;
    let error = client.submit_transaction(rpc, false).await.expect_err("node accepted oversized storage mass");
    let message = error.to_string();
    ensure!(
        message.to_ascii_lowercase().contains("storage") && message.contains(&storage.to_string()),
        "unexpected node rejection: {message}"
    );
    ensure!(client.get_mempool_entries(true, false).await?.is_empty());
    mine(client, miner, 3).await?;
    ensure!(snapshot(client, &source.address).await? == before, "rejected transfer changed source UTXOs");
    ensure!(snapshot(client, destination).await? == destination_before, "rejected transfer changed destination UTXOs");
    println!(
        "NODE_REJECTED case={label} amount={amount} source_balance={} source_utxos_preserved=true recipient_preserved=true mempool_empty=true reason={message}",
        before.balance
    );
    Ok(())
}

async fn settle(
    client: &GrpcClient,
    api: &CeleniumApi,
    miner: &Address,
    source: &Wallet,
    destination: &Address,
    amount: u64,
    label: &str,
    no_change: bool,
) -> Result<u64> {
    let before = snapshot(client, &source.address).await?;
    let destination_before = snapshot(client, destination).await?;
    ensure!(destination_before.balance == 0, "destination must be fresh");
    let prepared = source.prepare(api, destination, amount).await?;
    let signed = unpack(&prepared)?;
    let storage = inspect_signed(label, &signed)?;
    ensure!(storage <= SIMNET_PARAMS.block_mass_limits.storage);
    ensure!(signed.tx.inputs.len() == 1 && signed.tx.outputs.len() == if no_change { 1 } else { 2 });
    ensure!(signed.tx.outputs[0].value == amount && signed.tx.outputs[0].script_public_key == pay_to_address_script(destination));
    let fee = prepared.fee_pradha();
    let remaining = before.balance.checked_sub(amount).and_then(|value| value.checked_sub(fee)).context("incorrect fee")?;
    ensure!(!no_change || remaining == 0);
    let txid = api.submit_prepared_transfer(prepared).await?;
    ensure!(txid == signed.tx.id().to_string());
    let mempool = client.get_mempool_entries(false, false).await?;
    ensure!(
        mempool.iter().any(|entry| entry
            .transaction
            .verbose_data
            .as_ref()
            .is_some_and(|data| data.transaction_id.to_string() == txid)),
        "submitted transaction absent from mempool"
    );

    // Virtual acceptance and the address index can lag block submission.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        mine(client, miner, 1).await?;
        let outputs = client.get_utxos_by_addresses(vec![destination.clone(), source.address.clone()]).await?;
        let received = outputs.iter().any(|entry| {
            entry.outpoint.transaction_id.to_string() == txid
                && entry.utxo_entry.amount == amount
                && entry.utxo_entry.script_public_key == pay_to_address_script(destination)
        });
        let change_present = remaining == 0
            || outputs.iter().any(|entry| {
                entry.outpoint.transaction_id.to_string() == txid
                    && entry.utxo_entry.amount == remaining
                    && entry.utxo_entry.script_public_key == pay_to_address_script(&source.address)
            });
        let old_input_absent =
            outputs.iter().all(|entry| TransactionOutpoint::from(entry.outpoint.clone()) != signed.tx.inputs[0].previous_outpoint);
        if received
            && change_present
            && old_input_absent
            && client.get_balance_by_address(destination.clone()).await? == amount
            && client.get_balance_by_address(source.address.clone()).await? == remaining
        {
            break;
        }
        ensure!(Instant::now() < deadline, "transaction did not reach confirmed UTXO state: {txid}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ensure!(client.get_mempool_entries(true, false).await?.is_empty());
    println!(
        "CONFIRMED case={label} txid={txid} amount={amount} fee={fee} source_before={} source_after={remaining} recipient_balance={amount} output_utxo_present=true input_spent=true no_change={no_change}",
        before.balance
    );
    Ok(fee)
}

async fn spend_all_amount(client: &GrpcClient, source: &Wallet, destination: &Address) -> Result<u64> {
    let input = single_input(client, &source.address).await?;
    let signed = signed_transfer(&input, source, destination, input.1.amount, 0)?;
    let calculator = MassCalculator::new_with_consensus_params(&SIMNET_PARAMS);
    let mass = calculator.calc_non_contextual_masses(&signed.tx).normalized_max(&SIMNET_PARAMS.block_mass_cofactors());
    let estimate = client.get_fee_estimate().await?;
    let rate = estimate.low_buckets.first().map(|bucket| bucket.feerate).unwrap_or(0.25).max(0.25);
    ensure!(rate.is_finite() && rate > 0.0);
    let fee = (mass as f64 * rate).ceil();
    ensure!(fee < input.1.amount as f64, "fee exhausts balance");
    input.1.amount.checked_sub(fee as u64).context("fee exhausts balance")
}

fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

async fn connect(endpoint: &str) -> Result<GrpcClient> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match GrpcClient::connect_with_args(
            NotificationMode::Direct,
            format!("grpc://{endpoint}"),
            None,
            false,
            None,
            false,
            Some(500_000),
            Default::default(),
        )
        .await
        {
            Ok(client) => return Ok(client),
            Err(error) if Instant::now() >= deadline => return Err(error.into()),
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn exercise(directory: &Path) -> Result<()> {
    let floor = Wallet::new();
    let reserve = Wallet::new();
    let operational = Wallet::new();
    let recipient = Wallet::new();
    let onward = Wallet::new();
    let final_recipient = Wallet::new();
    let miner = Wallet::new();
    let mut overrides: OverrideParams = serde_json::from_str("{}")?;
    overrides.mining_reserve = Some(MiningReserveAllocation {
        floor_script: pay_to_address_script(&floor.address),
        faucet_script: pay_to_address_script(&reserve.address),
    });
    let params = SIMNET_PARAMS.override_params(overrides.clone());
    ensure!(SIMNET_PARAMS.storage_mass_amount_floor == 0 && params.storage_mass_amount_floor == 0);
    ensure!(params.storage_mass_parameter == SIMNET_PARAMS.storage_mass_parameter);
    ensure!(params.block_mass_limits.storage == SIMNET_PARAMS.block_mass_limits.storage);
    let config_path = directory.join("reserve-allocation.json");
    // Only genesis funding is customized; transaction rules retain simnet defaults.
    std::fs::write(&config_path, serde_json::to_vec(&overrides)?)?;
    let endpoint = format!("127.0.0.1:{}", port()?);
    let node = inprocess_node::InProcessNode::start_from_args(celeniumd_lib::args::Args {
        simnet: true,
        appdir: Some(directory.to_string_lossy().into_owned()),
        override_params_file: Some(config_path.to_string_lossy().into_owned()),
        rpclisten: Some(endpoint.parse()?),
        listen: Some(format!("127.0.0.1:{}", port()?).parse()?),
        utxoindex: true,
        disable_upnp: true,
        disable_dns_seeding: true,
        outbound_target: 0,
        inbound_limit: 0,
        rpc_max_clients: 8,
        async_threads: 2,
        enable_unsynced_mining: true,
        block_template_cache_lifetime: Some(0),
        ram_scale: 0.1,
        no_log_files: true,
        log_level: "warn".into(),
        ..Default::default()
    })?;
    let outcome = async {
        let client = connect(&endpoint).await?;
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        let work = async {
            let info = client.get_server_info().await?;
            ensure!(info.network_id.network_type == NetworkType::Simnet);
            let dag = client.get_block_dag_info().await?;
            ensure!(dag.virtual_daa_score == 0 && dag.pruning_point_hash == params.genesis.hash);
            let api = CeleniumApi::new(endpoint, None, rx).await?;
            let reserve_before = snapshot(&client, &reserve.address).await?;
            let floor_before = snapshot(&client, &floor.address).await?;
            ensure!(reserve_before.balance == mining_reserve::FAUCET_RESERVE && reserve_before.outputs.len() == 1);
            ensure!(floor_before.balance == mining_reserve::GENERAL_RESERVE);
            ensure!(snapshot(&client, &operational.address).await?.balance == 0);
            ensure!(api.funding_snapshot(&reserve.address.to_string()).await?.mature_pradha == reserve_before.balance);
            println!("NETWORK network=simnet server={} genesis={} reserve={} general_reserve={} operational=0 mined_blocks=0 storage_floor=0 storage_limit={} skip_pow={} funding=opt_in_genesis_allocation",
                info.server_version, params.genesis.hash, reserve_before.balance, floor_before.balance, params.block_mass_limits.storage, params.skip_proof_of_work);

            for amount in [100_000, 500_000] {
                reject(&client, &api, &miner.address, &reserve, &recipient.address, amount, "reserve_payout").await?;
            }
            let payout_fee = settle(&client, &api, &miner.address, &reserve, &recipient.address, PAYOUT, "reserve_payout", false).await?;
            for amount in [1_000_000, 2_000_000] {
                reject(&client, &api, &miner.address, &recipient, &onward.address, amount, "onward_with_change").await?;
            }
            let amount = spend_all_amount(&client, &recipient, &onward.address).await?;
            let onward_fee = settle(&client, &api, &miner.address, &recipient, &onward.address, amount, "onward_spend_all", true).await?;
            let final_amount = spend_all_amount(&client, &onward, &final_recipient.address).await?;
            let final_fee = settle(&client, &api, &miner.address, &onward, &final_recipient.address, final_amount, "onward_spend_all_again", true).await?;
            ensure!(snapshot(&client, &floor.address).await? == floor_before);
            ensure!(snapshot(&client, &reserve.address).await?.balance == reserve_before.balance - PAYOUT - payout_fee);
            ensure!(snapshot(&client, &operational.address).await?.balance == 0);
            ensure!(snapshot(&client, &recipient.address).await?.balance == 0 && snapshot(&client, &onward.address).await?.balance == 0);
            ensure!(final_amount + onward_fee + final_fee == PAYOUT);
            println!("RESULT payout_minimum={PAYOUT} reserve_payout_confirmed=true repeated_spend_all_confirmed=true arbitrary_partial_spend_safe=false general_reserve_unchanged=true operational=0 payout_fee={payout_fee} onward_fees=[{onward_fee},{final_fee}] final_received={final_amount} reward_ledger_tested=false");
            Ok::<_, anyhow::Error>(())
        }.await;
        let _ = shutdown.send(true);
        client.disconnect().await?;
        work
    }.await;
    inprocess_node::shutdown_inprocess(node).await;
    outcome
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let scratch = std::env::var_os("TMPDIR").context("TMPDIR must point to the active scratch directory")?;
    let directory = Path::new(&scratch).join(format!("dynamic-reward-payout-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let result = exercise(&directory).await;
    std::fs::remove_dir_all(&directory)?;
    result?;
    println!("DYNAMIC_REWARD_PAYOUT_SIMNET_PROOF_FINISHED");
    Ok(())
}
