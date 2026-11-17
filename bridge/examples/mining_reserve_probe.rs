use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{
    config::{
        genesis::MiningReserveAllocation,
        mining_reserve,
        params::{OverrideParams, SIMNET_PARAMS},
    },
    network::NetworkType,
};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use celenium_stratum_bridge::{
    CeleniumApi, FaucetEntry, FaucetSigner, FaucetState, faucet_funding,
    reservoir_funds::PayoutSource,
    treasury::{PendingTransfer, TREASURY_JOURNAL_PATH, TreasuryJournal},
};
use celenium_txscript::pay_to_address_script;
use secp256k1::{Keypair, SECP256K1};
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wallet() -> (Address, [u8; 32]) {
    let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(Prefix::Simnet, Version::PubKey, &pair.x_only_public_key().0.serialize()), pair.secret_bytes())
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

async fn mine(client: &GrpcClient, destination: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let before = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(destination.clone(), b"mining-reserve-proof".to_vec()).await?;
        let report = client.submit_block(template.block, false).await?;
        ensure!(matches!(report.report, SubmitBlockReport::Success), "simnet block rejected");
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= before {
            ensure!(Instant::now() < deadline, "DAA did not advance");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    Ok(())
}

async fn exercise(directory: &Path) -> Result<()> {
    let (floor, _) = wallet();
    let (reserve, reserve_key) = wallet();
    let (operational, _) = wallet();
    let (recipient, recipient_key) = wallet();
    let (participant, _) = wallet();
    let (onward_address, _) = wallet();
    let (miner, _) = wallet();
    let mut overrides: OverrideParams = serde_json::from_str("{}")?;
    overrides.mining_reserve =
        Some(MiningReserveAllocation { floor_script: pay_to_address_script(&floor), faucet_script: pay_to_address_script(&reserve) });
    let params = SIMNET_PARAMS.override_params(overrides.clone());
    let config_path = directory.join("reserve.json");
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
        let work = async {
            ensure!(client.get_server_info().await?.network_id.network_type == NetworkType::Simnet);
            let dag = client.get_block_dag_info().await?;
            ensure!(dag.pruning_point_hash == params.genesis.hash);
            ensure!(dag.virtual_daa_score == 0, "proof must start before mining");
            let (shutdown, rx) = tokio::sync::watch::channel(false);
            let api = CeleniumApi::new(endpoint.clone(), None, rx).await?;
            let operational_funds = api.funding_snapshot(&operational.to_string()).await?;
            let reserve_funds = api.funding_snapshot(&reserve.to_string()).await?;
            let floor_funds = api.funding_snapshot(&floor.to_string()).await?;
            ensure!(operational_funds.mature_pradha == 0);
            ensure!(reserve_funds.mature_pradha == mining_reserve::FAUCET_RESERVE);
            ensure!(floor_funds.mature_pradha == mining_reserve::GENERAL_RESERVE);
            let supply = client.get_coin_supply().await?;
            ensure!(supply.circulating_pradha == 19_000_000 * 100_000_000);
            println!("RESERVE_FUNDED genesis={} operational=0 reserve={} floor={} initial_supply={} mined_blocks=0 default_storage_rules=true", params.genesis.hash, reserve_funds.mature_pradha, floor_funds.mature_pradha, supply.circulating_pradha);
            let amount = 100_000_000;
            let mut value = serde_json::to_value(FaucetState::default())?;
            // A legacy liability exercises a standard-mass payout without changing daily reward.
            value["ledger"][recipient.to_string()] = serde_json::to_value(FaucetEntry {
                balance_pradha: amount, total_claimed_pradha: amount, ..Default::default()
            })?;
            let state = Mutex::new(serde_json::from_value::<FaucetState>(value)?);
            faucet_funding::refresh(&api, &state, &operational.to_string(), Some(&reserve.to_string())).await?;
            ensure!(state.lock().unwrap().claim(&participant.to_string(), "simnet-participant")?.balance_pradha == 1000);
            let reserve_signer = FaucetSigner::from_secret_hex_for_network(hex::encode(reserve_key), Prefix::Simnet)?;
            let small = faucet_funding::prepare_withdrawal(
                &api, &state, &operational.to_string(), None,
                Some((&reserve.to_string(), &reserve_signer)), &participant.to_string(), 1000, None,
            ).await;
            let error = small.expect_err("standard storage mass unexpectedly accepted a 1000-pradha payout");
            ensure!(error.to_string().contains("SmallTransferNotSupported"));
            ensure!(state.lock().unwrap().balance(&participant.to_string()).unwrap().balance_pradha == 1000);
            println!("SMALL_PAYOUT_BLOCKED amount=1000 ledger_preserved=true reason={error:#}");
            let (source, transfer) = faucet_funding::prepare_withdrawal(
                &api, &state, &operational.to_string(), None,
                Some((&reserve.to_string(), &reserve_signer)), &recipient.to_string(), amount, None,
            ).await?;
            ensure!(source == PayoutSource::Reserve);
            let fee = transfer.fee_pradha();
            let txid = transfer.transaction_id().to_string();
            let journal_path = Path::new(TREASURY_JOURNAL_PATH);
            let ledger_path = directory.join("faucet_ledger.json");
            state.lock().unwrap().save(&ledger_path)?;
            let journal = TreasuryJournal {
                pending: Some(PendingTransfer {
                    transaction_json: transfer.transaction_json()?, transaction_id: txid.clone(),
                    amount_pradha: amount, fee_pradha: fee, source: reserve.to_string(),
                    destination: recipient.to_string(), network: "simnet".into(), withdrawal: true,
                    anchor: Some(api.transfer_anchor().await?), input_entries: transfer.input_entries(),
                }), ..Default::default()
            };
            journal.save(journal_path)?;
            let mut restored = TreasuryJournal::load(journal_path)?;
            ensure!(!restored.can_prepare());
            ensure!(faucet_funding::refresh(&api, &state, &operational.to_string(), Some(&reserve.to_string())).await.is_err());
            let pending = restored.pending.as_ref().context("journal lost pending withdrawal")?;
            ensure!(pending.source == reserve.to_string() && pending.transaction_json == transfer.transaction_json()?);
            ensure!(api.submit_journaled_transfer(pending).await? == txid);
            let _duplicate = api.submit_journaled_transfer(pending).await;
            println!("RESERVE_PAYMENT_SUBMITTED txid={txid} amount={amount} fee={fee} operational=0 source=reserve journal_reloaded=true");
            mine(&client, &miner, 12).await?;
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let received = client.get_balance_by_address(recipient.clone()).await?;
                if received == amount { break; }
                ensure!(Instant::now() < deadline, "recipient did not receive reserve payment");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let outputs = client.get_utxos_by_addresses(vec![recipient.clone()]).await?;
            ensure!(outputs.iter().any(|entry| entry.outpoint.transaction_id.to_string() == txid && entry.utxo_entry.amount == amount));
            let after = api.funding_snapshot(&reserve.to_string()).await?;
            ensure!(after.mature_pradha == mining_reserve::FAUCET_RESERVE - amount - fee);
            ensure!(api.funding_snapshot(&operational.to_string()).await?.mature_pradha == 0);
            ensure!(api.funding_snapshot(&floor.to_string()).await?.mature_pradha == mining_reserve::GENERAL_RESERVE);
            let onward = api.prepare_transfer(&recipient.to_string(), &onward_address.to_string(), amount / 2, &hex::encode(recipient_key)).await?;
            api.submit_prepared_transfer(onward).await?;
            mine(&client, &miner, 12).await?;
            ensure!(!api.get_transaction_output_present(&recipient.to_string(), &txid).await?);
            let mut accepted = false;
            for _ in 0..10 {
                if api.journaled_transfer_accepted(restored.pending.as_mut().unwrap()).await? {
                    accepted = true;
                    break;
                }
                restored.save(journal_path)?;
                restored = TreasuryJournal::load(journal_path)?;
            }
            ensure!(accepted, "reserve payment was lost after recipient spent its output");
            let mut reloaded = FaucetState::load(&ledger_path)?;
            ensure!(reloaded.settle_withdrawal(&txid, &recipient.to_string(), amount, fee));
            reloaded.save(&ledger_path)?;
            let mut reloaded = FaucetState::load(&ledger_path)?;
            ensure!(reloaded.settle_withdrawal(&txid, &recipient.to_string(), amount, fee));
            ensure!(reloaded.balance(&recipient.to_string()).unwrap().balance_pradha == 0);
            ensure!(reloaded.total_outstanding() == 1000 && reloaded.fees_today() == fee);
            restored.pending = None;
            restored.last_txid = Some(txid.clone());
            restored.save(journal_path)?;
            faucet_funding::refresh(&api, &state, &operational.to_string(), Some(&reserve.to_string())).await?;
            println!("RESERVE_FAUCET_REPLAY same_tx=true duplicate_payment=false recipient_spent=true ledger_restart=true remaining_claims=1000 fee_debited_once=true no_worker_connected=true");
            let template = client.get_block_template(miner.clone(), vec![]).await?;
            let payload = &template.block.transactions.first().context("coinbase missing")?.payload;
            ensure!(payload.len() >= 16);
            let subsidy = u64::from_le_bytes(payload[8..16].try_into()?);
            let daa = template.block.header.daa_score;
            ensure!(subsidy == mining_reserve::mining_subsidy(daa));
            println!("RESERVE_PAYMENT_CONFIRMED txid={txid} recipient={amount} reserve_remaining={} floor_unchanged=true reduced_subsidy={subsidy} daa={daa}", after.mature_pradha);
            let _ = shutdown.send(true);
            Ok::<_, anyhow::Error>(())
        }.await;
        client.disconnect().await?;
        work
    }.await;
    inprocess_node::shutdown_inprocess(node).await;
    outcome
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let directory = std::env::temp_dir().join(format!("mining-reserve-proof-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let previous_directory = std::env::current_dir()?;
    std::env::set_current_dir(&directory)?;
    let outcome = exercise(&directory).await;
    std::env::set_current_dir(previous_directory)?;
    std::fs::remove_dir_all(&directory)?;
    outcome?;
    println!("MINING_RESERVE_SIMNET_PROOF_FINISHED");
    Ok(())
}
