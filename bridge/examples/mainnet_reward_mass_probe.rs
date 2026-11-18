use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{
    config::params::{MAINNET_PARAMS, Params, SIMNET_PARAMS},
    constants::{MAX_TX_IN_SEQUENCE_NUM, TX_VERSION},
    mass::MassCalculator,
    sign,
    subnets::SUBNETWORK_ID_NATIVE,
    tx::{SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry},
};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use celenium_txscript::pay_to_address_script;
use secp256k1::{Keypair, SECP256K1};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;
#[path = "../src/transfer.rs"]
#[allow(dead_code)]
mod transfer;

const AMOUNTS: [u64; 3] = [100_000, 250_000, 500_000];

fn emit(value: Value) {
    println!("{value}");
}

fn wallet(prefix: Prefix) -> (Address, [u8; 32]) {
    let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(prefix, Version::PubKey, &pair.x_only_public_key().0.serialize()), pair.secret_bytes())
}

fn mass(params: &Params, signed: &SignableTransaction) -> Result<Value> {
    let calculator = MassCalculator::new_with_consensus_params(params);
    let non = calculator.calc_non_contextual_masses(&signed.tx);
    let storage = calculator.calc_contextual_masses(&signed.as_verifiable()).context("incomputable storage mass")?.storage_mass;
    let total: u64 = signed.entries.iter().map(|e| e.as_ref().unwrap().amount).sum();
    let fee = total - signed.tx.outputs.iter().map(|o| o.value).sum::<u64>();
    Ok(json!({"inputs":signed.tx.inputs.len(), "input_total":total,
        "outputs":signed.tx.outputs.iter().map(|o| o.value).collect::<Vec<_>>(),
        "compute":non.compute_mass, "transient":non.transient_mass, "storage":storage,
        "fee":fee, "storage_limit":params.block_mass_limits.storage,
        "fits":storage <= params.block_mass_limits.storage && non.compute_mass <= params.block_mass_limits.compute
            && non.transient_mass <= params.block_mass_limits.transient}))
}

fn raw_transfer(
    inputs: &[(TransactionOutpoint, UtxoEntry)],
    source: &Address,
    destination: &Address,
    amount: u64,
    fee: u64,
    secret: &[u8; 32],
) -> Result<SignableTransaction> {
    let total: u64 = inputs.iter().map(|(_, e)| e.amount).sum();
    let change = total.checked_sub(amount).and_then(|n| n.checked_sub(fee)).context("insufficient input value")?;
    let mut outputs = vec![TransactionOutput::new(amount, pay_to_address_script(destination))];
    if change > 0 {
        outputs.push(TransactionOutput::new(change, pay_to_address_script(source)));
    }
    let tx = Transaction::new(
        TX_VERSION,
        inputs.iter().map(|(op, _)| TransactionInput::new(*op, vec![], MAX_TX_IN_SEQUENCE_NUM, 1)).collect(),
        outputs,
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    let signed = sign::sign_with_multiple_v2(
        SignableTransaction::with_entries(tx, inputs.iter().map(|(_, e)| e.clone()).collect()),
        &[*secret],
    )
    .fully_signed()?;
    sign::verify(&signed.as_verifiable())?;
    Ok(signed)
}

fn equal_mass_defaults() -> Result<()> {
    let a = MAINNET_PARAMS;
    let b = SIMNET_PARAMS;
    ensure!(a.mass_per_tx_byte == b.mass_per_tx_byte);
    ensure!(a.mass_per_script_pub_key_byte == b.mass_per_script_pub_key_byte);
    ensure!(a.mass_per_sig_op == b.mass_per_sig_op);
    ensure!(a.storage_mass_parameter == b.storage_mass_parameter);
    ensure!(a.storage_mass_amount_floor == 0 && b.storage_mass_amount_floor == 0);
    ensure!(a.block_mass_limits.compute == b.block_mass_limits.compute);
    ensure!(a.block_mass_limits.storage == b.block_mass_limits.storage);
    ensure!(a.block_mass_limits.transient == b.block_mass_limits.transient);
    ensure!(a.max_tx_inputs == b.max_tx_inputs && a.max_tx_outputs == b.max_tx_outputs);
    emit(json!({"phase":"defaults", "mainnet_simnet_mass_equal":true, "floor":0,
        "storage_parameter":a.storage_mass_parameter, "compute_limit":a.block_mass_limits.compute,
        "storage_limit":a.block_mass_limits.storage,"transient_limit":a.block_mass_limits.transient,
        "simnet_skip_pow":b.skip_proof_of_work, "consensus_overrides":false}));
    Ok(())
}

fn fixtures() -> Result<()> {
    let (source, secret) = wallet(Prefix::Mainnet);
    let (dest, _) = wallet(Prefix::Mainnet);
    for (name, amounts) in [("128x2079547", vec![2_079_547; 128]), ("1x50000000000000", vec![50_000_000_000_000])] {
        let inputs: Vec<_> = amounts
            .into_iter()
            .enumerate()
            .map(|(i, amount)| {
                (
                    TransactionOutpoint::new(celenium_hashes::Hash::from_u64_word(i as u64 + 1), 0),
                    UtxoEntry::new(amount, pay_to_address_script(&source), 1, true, None),
                )
            })
            .collect();
        for amount in AMOUNTS {
            let signed_one = raw_transfer(&inputs[..1], &source, &dest, amount, 509, &secret)?;
            emit(json!({"phase":"fixture_one_input", "synthetic_outpoints":true, "fixture":name,
                "amount":amount, "mass":mass(&MAINNET_PARAMS,&signed_one)?}));
            match transfer::build_transfer(&MAINNET_PARAMS, 10_000, &source, &dest, amount, None, 0.25, &secret, inputs.clone()) {
                Ok(signed) => emit(json!({"phase":"fixture_builder", "synthetic_outpoints":true, "fixture":name,
                    "amount":amount, "ok":true, "mass":mass(&MAINNET_PARAMS,&signed)?})),
                Err(error) => emit(json!({"phase":"fixture_builder", "synthetic_outpoints":true, "fixture":name,
                    "amount":amount, "ok":false, "error":format!("{error:#}")})),
            }
        }
    }
    Ok(())
}

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

async fn connect(rpc: &str) -> Result<GrpcClient> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match GrpcClient::connect_with_args(
            NotificationMode::Direct,
            format!("grpc://{rpc}"),
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

async fn mine(client: &GrpcClient, source: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let before = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(source.clone(), b"default-mainnet-mass-probe".to_vec()).await?;
        let result = client.submit_block(template.block, false).await?;
        ensure!(matches!(result.report, SubmitBlockReport::Success), "block rejected: {:?}", result.report);
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= before {
            ensure!(Instant::now() < deadline, "virtual DAA stalled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

async fn spendable(client: &GrpcClient, source: &Address) -> Result<Vec<(TransactionOutpoint, UtxoEntry)>> {
    let daa = client.get_block_dag_info().await?.virtual_daa_score;
    let mut utxos: Vec<_> = client
        .get_utxos_by_addresses(vec![source.clone()])
        .await?
        .into_iter()
        .filter(|u| !u.utxo_entry.is_coinbase || u.utxo_entry.block_daa_score + SIMNET_PARAMS.coinbase_maturity() <= daa)
        .map(|u| (u.outpoint.into(), u.utxo_entry.into()))
        .collect();
    utxos.sort_by_key(|(outpoint, _): &(TransactionOutpoint, UtxoEntry)| outpoint.to_string());
    Ok(utxos)
}

async fn settle(client: &GrpcClient, miner: &Address, dest: &Address, signed: &SignableTransaction, label: &str) -> Result<()> {
    let amount = signed.tx.outputs[0].value;
    let txid = client.submit_transaction((&signed.tx).into(), false).await.context("positive submission rejected")?;
    let entry = client.get_mempool_entry(txid, true, false).await?;
    emit(json!({"phase":"mempool", "label":label, "txid":txid.to_string(), "fee":entry.fee, "amount":amount}));
    mine(client, miner, 5).await?;
    let utxos = client.get_utxos_by_addresses(vec![dest.clone()]).await?;
    let balance = client.get_balance_by_address(dest.clone()).await?;
    ensure!(balance == amount, "receiver balance {balance} differs from {amount}");
    ensure!(utxos.iter().any(|u| u.outpoint.transaction_id == txid && u.utxo_entry.amount == amount), "receiver UTXO missing");
    emit(json!({"phase":"receiver_utxo", "label":label, "txid":txid.to_string(), "balance":balance,
        "utxo_count":utxos.len(), "verified":true}));
    Ok(())
}

async fn live_cases(client: &GrpcClient) -> Result<()> {
    ensure!(client.get_server_info().await?.network_id == SIMNET_PARAMS.net, "wrong network");
    let (source, secret) = wallet(Prefix::Simnet);
    mine(client, &source, 200 + SIMNET_PARAMS.coinbase_maturity()).await?;
    let initial = spendable(client, &source).await?;
    ensure!(initial.len() >= 128, "need 128 mined inputs");
    ensure!(initial.iter().all(|(_, e)| e.is_coinbase && e.amount == 2_079_547), "unexpected mined denomination");
    emit(json!({"phase":"funding", "origin":"locally_mined", "utxos":initial.len(), "input_amount":2_079_547,
        "balance":client.get_balance_by_address(source.clone()).await?, "maturity":SIMNET_PARAMS.coinbase_maturity()}));
    for amount in AMOUNTS {
        let (dest, dest_secret) = wallet(Prefix::Simnet);
        let inputs: Vec<_> = spendable(client, &source).await?.into_iter().filter(|(_, e)| e.is_coinbase).take(128).collect();
        let rejected = raw_transfer(&inputs[..1], &source, &dest, amount, 509, &secret)?;
        let rejection = client.submit_transaction((&rejected.tx).into(), false).await;
        ensure!(rejection.is_err(), "single ordinary input unexpectedly accepted");
        let error = format!("{:#}", rejection.unwrap_err());
        ensure!(error.to_lowercase().contains("mass"), "unexpected rejection {error}");
        ensure!(client.get_balance_by_address(dest.clone()).await? == 0);
        emit(json!({"phase":"node_negative", "kind":"one_mined_input", "amount":amount,
            "mass":mass(&SIMNET_PARAMS,&rejected)?, "error":error, "receiver_balance":0}));
        let daa = client.get_block_dag_info().await?.virtual_daa_score;
        let signed = transfer::build_transfer(&SIMNET_PARAMS, daa, &source, &dest, amount, None, 0.25, &secret, inputs)?;
        ensure!(mass(&MAINNET_PARAMS, &signed)? == mass(&SIMNET_PARAMS, &signed)?, "signed mass differs from mainnet");
        emit(json!({"phase":"node_builder", "amount":amount, "mainnet_mass_equal":true, "mass":mass(&MAINNET_PARAMS,&signed)?}));
        let label = format!("reward_{amount}");
        settle(client, &source, &dest, &signed, &label).await?;
        let (next, _) = wallet(Prefix::Simnet);
        let respent = transfer::build_transfer(
            &SIMNET_PARAMS,
            client.get_block_dag_info().await?.virtual_daa_score,
            &dest,
            &next,
            amount - 509,
            Some(509),
            0.25,
            &dest_secret,
            spendable(client, &dest).await?,
        )?;
        emit(json!({"phase":"respend_mass", "reward":amount, "mass":mass(&MAINNET_PARAMS,&respent)?}));
        settle(client, &source, &next, &respent, &format!("respend_{amount}")).await?;
    }
    let inputs: Vec<_> = spendable(client, &source).await?.into_iter().filter(|(_, e)| e.is_coinbase).take(64).collect();
    ensure!(inputs.len() == 64);
    let total: u64 = inputs.iter().map(|(_, e)| e.amount).sum();
    let (reserve, reserve_secret) = wallet(Prefix::Simnet);
    let reserve_tx = transfer::build_transfer(
        &SIMNET_PARAMS,
        client.get_block_dag_info().await?.virtual_daa_score,
        &source,
        &reserve,
        total - 100_000,
        Some(100_000),
        0.25,
        &secret,
        inputs,
    )?;
    settle(client, &source, &reserve, &reserve_tx, "consolidated_mined_reserve").await?;
    let reserve_inputs = spendable(client, &reserve).await?;
    ensure!(reserve_inputs.len() == 1);
    for amount in AMOUNTS {
        let (dest, _) = wallet(Prefix::Simnet);
        let signed = raw_transfer(&reserve_inputs, &reserve, &dest, amount, 509, &reserve_secret)?;
        let result = client.submit_transaction((&signed.tx).into(), false).await;
        ensure!(result.is_err(), "consolidated input tiny payout unexpectedly accepted");
        let error = format!("{:#}", result.unwrap_err());
        ensure!(error.to_lowercase().contains("mass"), "unexpected rejection {error}");
        emit(json!({"phase":"node_negative", "kind":"one_consolidated_mined_input", "amount":amount,
            "mass":mass(&MAINNET_PARAMS,&signed)?, "error":error, "receiver_balance":client.get_balance_by_address(dest).await?}));
    }
    Ok(())
}

async fn live(directory: &Path) -> Result<()> {
    let rpc = format!("127.0.0.1:{}", free_port()?);
    let args = celeniumd_lib::args::Args {
        simnet: true,
        appdir: Some(directory.to_string_lossy().into_owned()),
        rpclisten: Some(rpc.parse()?),
        listen: Some(format!("127.0.0.1:{}", free_port()?).parse()?),
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
        log_level: "warn".to_string(),
        ..Default::default()
    };
    ensure!(args.override_params_file.is_none(), "consensus overrides forbidden");
    let node = inprocess_node::InProcessNode::start_from_args(args)?;
    let result = async {
        let client = connect(&rpc).await?;
        let result = tokio::time::timeout(Duration::from_secs(180), live_cases(&client)).await;
        client.disconnect().await?;
        result?
    }
    .await;
    inprocess_node::shutdown_inprocess(node).await;
    result
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    equal_mass_defaults()?;
    fixtures()?;
    if std::env::args().any(|a| a == "--mass-only") {
        return Ok(());
    }
    let scratch = std::env::var("TMPDIR").context("TMPDIR must point to the active scratch directory")?;
    ensure!(scratch.starts_with("/home/dx401/.hermes/profiles/devops/cache/scratch"));
    let directory = Path::new(&scratch).join(format!("default-mass-chain-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = live(&directory).await;
    std::fs::remove_dir_all(&directory)?;
    result?;
    emit(json!({"phase":"complete", "live_node_verified":true,"chain_removed":true,"ephemeral_secrets_persisted":false}));
    Ok(())
}
