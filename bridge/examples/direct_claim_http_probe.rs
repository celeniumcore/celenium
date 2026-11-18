use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::config::params::{MAINNET_PARAMS, SIMNET_PARAMS};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use secp256k1::{Keypair, SECP256K1};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wallet() -> (Address, [u8; 32]) {
    let key = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(Prefix::Simnet, Version::PubKey, &key.x_only_public_key().0.serialize()), key.secret_bytes())
}

struct BridgeProcess(Child);
impl Drop for BridgeProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_bridge(binary: &Path, directory: &Path, source_secret: &[u8; 32]) -> Result<BridgeProcess> {
    let log = std::fs::OpenOptions::new().create(true).append(true).open(directory.join("bridge.log"))?;
    let child = Command::new(binary)
        .args(["--config", "config.yaml", "--node-mode", "external", "--skip-sync-wait"])
        .current_dir(directory)
        .env_remove("CELENIUM_FAUCET_KEYFILE")
        .env_remove("CELENIUM_FAUCET_RESERVE_KEYFILE")
        .env("CELENIUM_FAUCET_PRIVKEY", hex::encode(source_secret))
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn()?;
    Ok(BridgeProcess(child))
}

fn http(port: u16, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut stream = TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse()?, Duration::from_secs(2))?;
    stream.set_read_timeout(Some(Duration::from_secs(25)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    ensure!(response.starts_with("HTTP/1.1 200"), "unexpected HTTP status");
    Ok(serde_json::from_str(response.split_once("\r\n\r\n").context("missing HTTP body")?.1)?)
}

async fn bridge_ready(port: u16) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        if http(port, "GET", "/api/status", None).is_ok() {
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "bridge HTTP startup timed out");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
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
            Err(_) => tokio::time::sleep(Duration::from_millis(150)).await,
        }
    }
}

async fn mine(client: &GrpcClient, address: &Address, count: usize) -> Result<()> {
    for _ in 0..count {
        let before = client.get_block_dag_info().await?.virtual_daa_score;
        let template = client.get_block_template(address.clone(), b"direct-claim-http-local".to_vec()).await?;
        ensure!(matches!(client.submit_block(template.block, false).await?.report, SubmitBlockReport::Success));
        let deadline = Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= before {
            ensure!(Instant::now() < deadline, "node virtual DAA stalled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    Ok(())
}

fn txid(value: &Value) -> Result<String> {
    let txid = value["txid"].as_str().context("claim returned no TXID")?;
    ensure!(txid.len() == 64 && txid.bytes().all(|b| b.is_ascii_hexdigit()), "malformed TXID");
    Ok(txid.into())
}

async fn exercise(directory: &Path, binary: &Path) -> Result<()> {
    ensure!(MAINNET_PARAMS.storage_mass_amount_floor == 0 && SIMNET_PARAMS.storage_mass_amount_floor == 0);
    ensure!(MAINNET_PARAMS.storage_mass_parameter == SIMNET_PARAMS.storage_mass_parameter);
    ensure!(MAINNET_PARAMS.block_mass_limits.storage == SIMNET_PARAMS.block_mass_limits.storage);
    let (source, secret) = wallet();
    let (recipient, _) = wallet();
    let rpc = format!("127.0.0.1:{}", free_port()?);
    let web = free_port()?;
    let stratum = free_port()?;
    let args = celeniumd_lib::args::Args {
        simnet: true,
        appdir: Some(directory.join("chain").to_string_lossy().into_owned()),
        rpclisten: Some(rpc.parse()?),
        listen: Some(format!("127.0.0.1:{}", free_port()?).parse()?),
        utxoindex: true,
        disable_upnp: true,
        disable_dns_seeding: true,
        outbound_target: 0,
        inbound_limit: 0,
        rpc_max_clients: 12,
        async_threads: 2,
        enable_unsynced_mining: true,
        block_template_cache_lifetime: Some(0),
        ram_scale: 0.1,
        no_log_files: true,
        log_level: "warn".into(),
        ..Default::default()
    };
    ensure!(args.override_params_file.is_none());
    let node = inprocess_node::InProcessNode::start_from_args(args)?;
    let result = async {
        let client = connect(&rpc).await?;
        let result = async {
            ensure!(client.get_server_info().await?.network_id == SIMNET_PARAMS.net);
            println!("{}", json!({"phase":"network", "network":"isolated-simnet", "mainnet_mass_defaults":true, "consensus_overrides":false, "pow_bypassed":true}));
            let config = format!("celeniumd_address: \"{rpc}\"\nweb_dashboard_port: \"127.0.0.1:{web}\"\nprint_stats: false\nlog_to_file: false\nfaucet_address: \"{source}\"\ninstances:\n  - stratum_port: \"127.0.0.1:{stratum}\"\n    min_share_diff: 0.00006103515625\n    log_to_file: false\n");
            std::fs::write(directory.join("config.yaml"), config)?;
            let mut bridge = Some(start_bridge(binary, directory, &secret)?);
            bridge_ready(web).await?;
            let request = json!({"address": recipient.to_string(), "deviceId":"local-direct-claim-test"});
            let empty = http(web, "POST", "/api/faucet/claim", Some(&request))?;
            ensure!(empty["ok"] != true && empty["txid"].is_null(), "unfunded claim must not pretend payment");
            let preview = http(web, "GET", &format!("/api/faucet/balance?address={recipient}"), None)?;
            ensure!(preview["balancePradha"] == 0 && preview["totalClaimedPradha"] == 0);
            println!("{}", json!({"phase":"unfunded", "response":empty, "ledger_unchanged":true}));
            mine(&client, &source, 220).await?;
            let first = http(web, "POST", "/api/faucet/claim", Some(&request))?;
            ensure!(first["ok"] == true && first["payoutStatus"] == "submitted", "claim did not submit: {first}");
            ensure!(first["claimPayoutMode"] == "on-chain");
            let first_txid = txid(&first)?;
            let paid = first["paidPradha"].as_u64().context("missing paid amount")?;
            ensure!((100_000..=500_000).contains(&paid) && first["balancePradha"] == 0);
            client.get_mempool_entry(first_txid.parse()?, true, false).await?;
            println!("{}", json!({"phase":"claim_http_submitted", "response":first, "mempool_verified":true}));
            let (second_recipient, _) = wallet();
            let second = http(web, "POST", "/api/faucet/claim", Some(&json!({"address":second_recipient.to_string(),"deviceId":"second-local-device"})))?;
            ensure!(second["ok"] == true && second["payoutStatus"] == "submitted", "first mempool payment blocked a second wallet: {second}");
            let second_txid = txid(&second)?;
            ensure!(second_txid != first_txid);
            client.get_mempool_entry(second_txid.parse()?, true, false).await?;
            println!("{}", json!({"phase":"second_wallet_before_confirmation", "txid":second_txid,"independent_payment_accepted":true}));
            mine(&client, &source, 15).await?;
            let balance = client.get_balance_by_address(recipient.clone()).await?;
            ensure!(balance == paid);
            let utxos = client.get_utxos_by_addresses(vec![recipient.clone()]).await?;
            ensure!(utxos.iter().any(|entry| entry.outpoint.transaction_id.to_string() == first_txid && entry.utxo_entry.amount == paid));
            let repeated = http(web, "POST", "/api/faucet/claim", Some(&request))?;
            ensure!(repeated["ok"] == true && txid(&repeated)? == first_txid, "lost-response retry did not recover original receipt: {repeated}");
            drop(bridge.take());
            bridge = Some(start_bridge(binary, directory, &secret)?);
            bridge_ready(web).await?;
            let replayed = http(web, "POST", "/api/faucet/claim", Some(&request))?;
            ensure!(replayed["ok"] == true && txid(&replayed)? == first_txid, "restart lost payment receipt: {replayed}");
            mine(&client, &source, 5).await?;
            ensure!(client.get_balance_by_address(recipient.clone()).await? == paid, "retries paid the recipient twice");
            let final_balance = http(web, "GET", &format!("/api/faucet/balance?address={recipient}"), None)?;
            ensure!(final_balance["balancePradha"] == 0 && final_balance["totalClaimedPradha"] == paid);
            println!("{}", json!({"phase":"complete", "http_claim_calls":4, "txid":first_txid, "paidPradha":paid, "receiver_utxo_verified":true, "retry_same_txid":true, "restart_same_txid":true, "duplicate_payment":false, "no_separate_withdraw_request":true}));
            drop(bridge.take());
            Ok::<_, anyhow::Error>(())
        }.await;
        client.disconnect().await?;
        result
    }.await;
    inprocess_node::shutdown_inprocess(node).await;
    result
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let scratch = PathBuf::from(std::env::var("TMPDIR").context("TMPDIR required")?);
    ensure!(scratch.starts_with("/home/dx401/.hermes/profiles/devops/cache/scratch"));
    let directory = scratch.join(format!("direct-claim-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let binary =
        std::env::var_os("CELENIUM_PROBE_BRIDGE_BIN").map(PathBuf::from).unwrap_or_else(|| repo.join("target/debug/stratum-bridge"));
    ensure!(binary.is_file(), "build the bridge binary first");
    let result = exercise(&directory, &binary).await;
    if result.is_ok() {
        std::fs::remove_dir_all(&directory)?;
        println!("{}", json!({"phase":"cleanup", "isolated_chain_removed":true, "ephemeral_private_keys_persisted":false}));
    } else {
        eprintln!("Probe state retained without private keys at {}", directory.display());
    }
    result
}
