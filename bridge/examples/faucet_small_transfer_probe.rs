use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{
    config::params::{Params, SIMNET_PARAMS},
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
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

const FLOOR: u64 = 4_000_000;

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wallet() -> (Address, [u8; 32]) {
    let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(Prefix::Simnet, Version::PubKey, &pair.x_only_public_key().0.serialize()), pair.secret_bytes())
}

fn transfer(
    input: &(TransactionOutpoint, UtxoEntry),
    source: &Address,
    destination: &Address,
    amount: u64,
    fee: u64,
    secret: &[u8; 32],
) -> Result<SignableTransaction> {
    let change = input.1.amount.checked_sub(amount).and_then(|n| n.checked_sub(fee)).context("insufficient funding")?;
    let mut outputs = vec![TransactionOutput::new(amount, pay_to_address_script(destination))];
    if change > 0 {
        outputs.push(TransactionOutput::new(change, pay_to_address_script(source)));
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
    let signed = sign::sign_with_multiple_v2(SignableTransaction::with_entries(tx, vec![input.1.clone()]), &[*secret]);
    let sign::Signed::Fully(signed) = signed else { anyhow::bail!("partial signature") };
    sign::verify(&signed.as_verifiable())?;
    Ok(signed)
}

fn fee_and_mass(params: &Params, signed: &SignableTransaction) -> Result<(u64, u64)> {
    let calc = MassCalculator::new_with_consensus_params(params);
    let non = calc.calc_non_contextual_masses(&signed.tx);
    let storage = calc.calc_contextual_masses(&signed.as_verifiable()).context("incomputable mass")?.storage_mass;
    let fee = non.compute_mass.max(non.normalized_transient(&params.block_mass_cofactors())).saturating_mul(250) / 1000;
    println!(
        "MASS floor={} amount={} inputs={} outputs={} compute={} transient={} storage={storage} limit={} relay_fee={fee}",
        params.storage_mass_amount_floor,
        signed.tx.outputs[0].value,
        signed.tx.inputs.len(),
        signed.tx.outputs.len(),
        non.compute_mass,
        non.transient_mass,
        params.block_mass_limits.storage
    );
    Ok((fee, storage))
}

async fn mine(client: &GrpcClient, source: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let previous = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(source.clone(), b"storage-floor-local-proof".to_vec()).await?;
        let report = client.submit_block(template.block, false).await?;
        ensure!(matches!(report.report, SubmitBlockReport::Success), "block rejected: {:?}", report.report);
        let start = Instant::now();
        while client.get_block_dag_info().await?.virtual_daa_score <= previous {
            ensure!(start.elapsed() < Duration::from_secs(10), "virtual DAA stalled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
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
            Err(_) => tokio::time::sleep(Duration::from_millis(250)).await,
        }
    }
}

fn args(directory: &Path, rpc: &str, override_file: Option<String>) -> Result<celeniumd_lib::args::Args> {
    Ok(celeniumd_lib::args::Args {
        simnet: true,
        appdir: Some(directory.to_string_lossy().to_string()),
        override_params_file: override_file,
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
    })
}

async fn funded_input(client: &GrpcClient, source: &Address) -> Result<(TransactionOutpoint, UtxoEntry)> {
    let daa = client.get_block_dag_info().await?.virtual_daa_score;
    let utxos = client.get_utxos_by_addresses(vec![source.clone()]).await?;
    let u = utxos
        .iter()
        .find(|u| !u.utxo_entry.is_coinbase || u.utxo_entry.block_daa_score + SIMNET_PARAMS.coinbase_maturity() <= daa)
        .context("no mature mined input")?;
    println!(
        "FUNDED virtual_daa={daa} utxos={} source_balance={} input_amount={} coinbase={} maturity={}",
        utxos.len(),
        client.get_balance_by_address(source.clone()).await?,
        u.utxo_entry.amount,
        u.utxo_entry.is_coinbase,
        SIMNET_PARAMS.coinbase_maturity()
    );
    Ok((u.outpoint.into(), u.utxo_entry.clone().into()))
}

async fn run(directory: &Path) -> Result<()> {
    let (source, source_secret) = wallet();
    let (destination, destination_secret) = wallet();
    let (micro_destination, _) = wallet();
    let rpc = format!("127.0.0.1:{}", free_port()?);
    let override_path = directory.join("storage-floor.json");
    std::fs::write(&override_path, format!("{{\"storage_mass_amount_floor\":{FLOOR}}}"))?;
    let node =
        inprocess_node::InProcessNode::start_from_args(args(directory, &rpc, Some(override_path.to_string_lossy().to_string()))?)?;
    let opt_in = async {
        let client = connect(&rpc).await?;
        let result = async {
            let mut params = SIMNET_PARAMS;
            params.storage_mass_amount_floor = FLOOR;
            ensure!(client.get_server_info().await?.network_id.network_type == celenium_consensus_core::network::NetworkType::Simnet);
            println!("NETWORK simnet opt_in_floor={FLOOR} production_builder=true skip_pow={}", params.skip_proof_of_work);
            mine(&client, &source, params.coinbase_maturity() + 10).await?;
            let input = funded_input(&client, &source).await?;
            let signed = transfer(&input, &source, &destination, 1000, 509, &source_secret)?;
            let (_, legacy_mass) = fee_and_mass(&SIMNET_PARAMS, &signed)?;
            let (_, mass) = fee_and_mass(&params, &signed)?;
            ensure!(legacy_mass > SIMNET_PARAMS.block_mass_limits.storage);
            ensure!(mass == 250_000 && mass <= params.block_mass_limits.storage);
            let (shutdown, rx) = tokio::sync::watch::channel(false);
            let api = celenium_stratum_bridge::CeleniumApi::new(rpc.clone(), None, rx).await?;
            let mut ledger = celenium_stratum_bridge::FaucetState::default();
            let claim = ledger.claim(&destination.to_string(), "isolated-test-device")?;
            ensure!(claim.balance_pradha == 1000 && claim.last_claim_payout_pradha == 1000);
            let ledger_path = directory.join("faucet-ledger.json");
            ledger.save(&ledger_path)?;
            ensure!(ledger.claim(&destination.to_string(), "isolated-test-device").is_err());
            println!("CLAIM_CONFIRMED ledger_balance={} duplicate_rejected=true", claim.balance_pradha);
            let quote = api
                .prepare_simnet_transfer(&source.to_string(), &destination.to_string(), 1000, &hex::encode(source_secret), FLOOR)
                .await?;
            let fee = quote.fee_pradha();
            ledger.check_withdraw_fee(&destination.to_string(), fee)?;
            let txid = api.submit_prepared_transfer(quote).await?;
            ensure!(ledger.record_withdraw(&destination.to_string(), 1000, fee));
            ledger.save(&ledger_path)?;
            mine(&client, &source, 5).await?;
            let received = client.get_balance_by_address(destination.clone()).await?;
            let outputs = client.get_utxos_by_addresses(vec![destination.clone()]).await?;
            ensure!(received == 1000);
            ensure!(outputs.iter().any(|u| u.outpoint.transaction_id.to_string() == txid && u.utxo_entry.amount == 1000));
            ensure!(ledger.balance(&destination.to_string()).unwrap().balance_pradha == 0);
            println!("FAUCET_CONFIRMED txid={txid} wallet_balance={received} ledger_balance=0 fee={fee}");
            let quote = api
                .prepare_simnet_transfer(
                    &destination.to_string(),
                    &micro_destination.to_string(),
                    1,
                    &hex::encode(destination_secret),
                    FLOOR,
                )
                .await?;
            let micro_fee = quote.fee_pradha();
            let micro_txid = api.submit_prepared_transfer(quote).await?;
            mine(&client, &source, 5).await?;
            let received = client.get_balance_by_address(micro_destination.clone()).await?;
            let remaining = client.get_balance_by_address(destination.clone()).await?;
            ensure!(received == 1 && remaining == 1000 - 1 - micro_fee);
            println!("MICRO_CONFIRMED txid={micro_txid} wallet_balance={received} fee={micro_fee} source_remaining={remaining}");
            let _ = shutdown.send(true);
            Ok::<_, anyhow::Error>(())
        }
        .await;
        client.disconnect().await?;
        result
    }
    .await;
    inprocess_node::shutdown_inprocess(node).await;
    opt_in
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let directory = std::env::temp_dir().join(format!("storage-floor-proof-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = run(&directory).await;
    std::fs::remove_dir_all(&directory)?;
    result?;
    println!("STORAGE_FLOOR_SIMNET_PROOF_FINISHED");
    Ok(())
}
