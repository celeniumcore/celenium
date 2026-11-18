use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{config::params::SIMNET_PARAMS, network::NetworkType};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use celenium_stratum_bridge::{
    CeleniumApi, FaucetState, reservoir_funds,
    treasury::{PendingTransfer, TreasuryJournal},
};
use secp256k1::{Keypair, SECP256K1};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wallet() -> (Address, [u8; 32]) {
    let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(Prefix::Simnet, Version::PubKey, &pair.x_only_public_key().0.serialize()), pair.secret_bytes())
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
            Err(e) if Instant::now() >= deadline => return Err(e.into()),
            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
        }
    }
}

async fn mine(client: &GrpcClient, source: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let previous = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(source.clone(), b"reservoir-local-proof".to_vec()).await?;
        let report = client.submit_block(template.block, false).await?;
        ensure!(matches!(report.report, SubmitBlockReport::Success), "local block rejected");
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= previous {
            ensure!(Instant::now() < deadline, "virtual DAA stalled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

async fn exercise(directory: &Path, rpc: &str) -> Result<()> {
    let client = connect(rpc).await?;
    let result = async {
        let (source, secret) = wallet();
        let (miner_fund, miner_secret) = wallet();
        let (participant, _) = wallet();
        ensure!(client.get_server_info().await?.network_id.network_type == NetworkType::Simnet);
        println!("NETWORK simnet default_params=true skip_pow={} maturity={}", SIMNET_PARAMS.skip_proof_of_work, SIMNET_PARAMS.coinbase_maturity());
        let (shutdown, rx) = tokio::sync::watch::channel(false);
        let api = CeleniumApi::new(rpc.into(), None, rx).await?;
        let original_anchor = api.transfer_anchor().await?;
        let empty = api.funding_snapshot(&source.to_string()).await?;
        ensure!(empty.mature_pradha == 0);
        let mut ledger = FaucetState::default();
        ledger.update_funding(0, empty.month, empty.observed_at);
        ensure!(ledger.claim(&participant.to_string(), "test-device").is_err());
        println!("UNFUNDED new_claim_rejected=true");
        mine(&client, &source, 2700).await?;
        let funds = api.funding_snapshot(&source.to_string()).await?;
        ensure!(funds.mature_pradha > reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 100_000);
        ledger.update_funding(funds.mature_pradha, funds.month, funds.observed_at);
        let claim = ledger.claim(&participant.to_string(), "test-device")?;
        ensure!((100_000..=500_000).contains(&claim.balance_pradha));
        let reserve = ledger.service_reserve_pradha();
        // Zero floor exercises the end-emission boundary without changing consensus.
        let requested = reservoir_funds::sweep_amount_with_service_reserve(funds.mature_pradha, 0, ledger.total_outstanding(), reserve, 1_000_000);
        ensure!(requested > 400_000_000);
        let quote = api.prepare_surplus_transfer(&source.to_string(), &miner_fund.to_string(), requested, &hex::encode(secret)).await?;
        let transaction: celenium_rpc_core::RpcTransaction = serde_json::from_str(&quote.transaction_json()?)?;
        let amount = transaction.outputs[0].value;
        ensure!(amount < requested && amount > 0 && transaction.inputs.len() <= 128, "surplus did not fit an input batch");
        let fee = quote.fee_pradha();
        ensure!(amount <= reservoir_funds::sweep_amount_with_service_reserve(funds.mature_pradha, 0, ledger.total_outstanding(), reserve, fee));
        let txid = quote.transaction_id().to_owned();
        let journal_path = directory.join("reservoir-journal.json");
        let journal = TreasuryJournal {
            pending: Some(PendingTransfer {transaction_json: quote.transaction_json()?, transaction_id: txid.clone(), amount_pradha: amount, fee_pradha: fee, source: source.to_string(), destination: miner_fund.to_string(), network: "simnet".into(), withdrawal: false, anchor: Some(original_anchor), input_entries: quote.input_entries()}),
            ..Default::default()
        };
        journal.save(&journal_path)?;
        let mut restored = TreasuryJournal::load(&journal_path)?;
        ensure!(!restored.can_prepare());
        api.submit_journaled_transfer(restored.pending.as_ref().context("pending lost")?).await?;
        let pending_funds = api.funding_snapshot(&source.to_string()).await?;
        ensure!(pending_funds.mature_pradha < funds.mature_pradha, "mempool-spent inputs were counted again");
        let _duplicate = api.submit_journaled_transfer(restored.pending.as_ref().unwrap()).await;
        mine(&client, &source, 15).await?;
        ensure!(api.get_transaction_output_present(&miner_fund.to_string(), &txid).await?);
        let received = client.get_balance_by_address(miner_fund.clone()).await?;
        ensure!(received == amount, "journal replay paid a second amount");
        let after = api.funding_snapshot(&source.to_string()).await?;
        ensure!(after.mature_pradha >= reservoir_funds::protected_balance_with_service_reserve(0, ledger.total_outstanding(), reserve));
        ensure!(ledger.balance(&participant.to_string()).unwrap().balance_pradha == claim.balance_pradha);
        let onward = api.prepare_transfer(&miner_fund.to_string(), &participant.to_string(), amount / 2, &hex::encode(miner_secret)).await?;
        api.submit_prepared_transfer(onward).await?;
        mine(&client, &source, 15).await?;
        ensure!(!api.get_transaction_output_present(&miner_fund.to_string(), &txid).await?);
        let mut pages = 0;
        let mut accepted = false;
        for _ in 0..10 {
            pages += 1;
            if api.journaled_transfer_accepted(restored.pending.as_mut().unwrap()).await? { accepted = true; break }
            restored.save(&journal_path)?;
            restored = TreasuryJournal::load(&journal_path)?;
        }
        ensure!(accepted && pages > 1, "accepted payment lost beyond the first history page");
        let next_funds = api.funding_snapshot(&source.to_string()).await?;
        let next_maximum = reservoir_funds::sweep_amount_with_service_reserve(next_funds.mature_pradha, 0, ledger.total_outstanding(), reserve, 1_000_000);
        let next = api.prepare_surplus_transfer(&source.to_string(), &miner_fund.to_string(), next_maximum, &hex::encode(secret)).await?;
        let next_tx: celenium_rpc_core::RpcTransaction = serde_json::from_str(&next.transaction_json()?)?;
        let second_amount = next_tx.outputs[0].value;
        api.submit_prepared_transfer(next).await?;
        mine(&client, &source, 15).await?;
        let second_received = client.get_balance_by_address(miner_fund.clone()).await?;
        ensure!(second_received > second_amount, "second surplus batch did not arrive");
        println!("SURPLUS_CONFIRMED txid={txid} amount={amount} fee={fee} received={received} protected={} source_mature={} outstanding={} duplicate_payment=false", reservoir_funds::protected_balance_with_service_reserve(0, ledger.total_outstanding(), reserve), after.mature_pradha, ledger.total_outstanding());
        println!("JOURNAL_RESTART pending_preserved=true mempool_spends_excluded=true default_consensus=true history_pages={pages} requested={requested} second_batch={second_amount}");
        let _ = shutdown.send(true);
        Ok::<_, anyhow::Error>(())
    }.await;
    client.disconnect().await?;
    result
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let directory = std::env::temp_dir().join(format!("reservoir-proof-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let rpc = format!("127.0.0.1:{}", free_port()?);
    let args = celeniumd_lib::args::Args {
        simnet: true,
        appdir: Some(directory.to_string_lossy().to_string()),
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
        log_level: "warn".into(),
        ..Default::default()
    };
    let node = inprocess_node::InProcessNode::start_from_args(args)?;
    let result = exercise(&directory, &rpc).await;
    inprocess_node::shutdown_inprocess(node).await;
    std::fs::remove_dir_all(&directory)?;
    result?;
    println!("RESERVOIR_SIMNET_PROOF_FINISHED");
    Ok(())
}
