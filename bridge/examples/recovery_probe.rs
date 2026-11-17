use anyhow::{Result, ensure};
use celenium_addresses::{Address, Prefix, Version};
use celenium_consensus_core::{
    config::{genesis::MiningReserveAllocation, params::OverrideParams},
    network::NetworkType,
};
use celenium_grpc_client::GrpcClient;
use celenium_rpc_core::{SubmitBlockReport, api::rpc::RpcApi, notify::mode::NotificationMode};
use celenium_stratum_bridge::{CeleniumApi, FaucetSigner, FaucetState, prom, treasury};
use celenium_txscript::pay_to_address_script;
use secp256k1::{Keypair, SECP256K1};
use std::{io::{BufRead, Write}, path::Path, sync::{Arc, Mutex}, time::Duration};

#[path = "../src/inprocess_node.rs"]
mod inprocess_node;

#[derive(serde::Serialize, serde::Deserialize)]
struct Identity {
    operational: String,
    reserve: String,
    reserve_secret: String,
    floor: String,
    miner: String,
    onward: String,
    recipients: Vec<(String, String)>,
    participants: Vec<String>,
}

fn wallet() -> (String, String) {
    let pair = Keypair::new(SECP256K1, &mut secp256k1::rand::thread_rng());
    (Address::new(Prefix::Simnet, Version::PubKey, &pair.x_only_public_key().0.serialize()).to_string(), hex::encode(pair.secret_bytes()))
}

fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn emit(value: serde_json::Value) {
    println!("RECOVERY {}", value);
    std::io::stdout().flush().unwrap();
}

async fn connect(rpc: &str) -> Result<GrpcClient> {
    for _ in 0..150 {
        if let Ok(client) = GrpcClient::connect_with_args(NotificationMode::Direct, format!("grpc://{rpc}"), None, false, None, false, Some(500_000), Default::default()).await {
            return Ok(client);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    anyhow::bail!("isolated simnet RPC did not start")
}

async fn mine(client: &GrpcClient, address: &Address, count: u64) -> Result<()> {
    for _ in 0..count {
        let before = client.get_block_dag_info().await?.virtual_daa_score;
        let block = client.get_block_template(address.clone(), b"recovery-proof".to_vec()).await?.block;
        ensure!(matches!(client.submit_block(block, false).await?.report, SubmitBlockReport::Success));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while client.get_block_dag_info().await?.virtual_daa_score <= before {
            ensure!(tokio::time::Instant::now() < deadline, "DAA stalled");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    Ok(())
}

fn identity(path: &Path) -> Result<Identity> {
    Ok(serde_json::from_slice(&std::fs::read(path.join("identity.json"))?)?)
}

async fn node(directory: &Path) -> Result<()> {
    let (reserve, reserve_secret) = wallet();
    let ids = Identity {
        operational: wallet().0, reserve, reserve_secret, floor: wallet().0,
        miner: wallet().0, onward: wallet().0,
        recipients: (0..4).map(|_| wallet()).collect(),
        participants: (0..4).map(|_| wallet().0).collect(),
    };
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(directory.join("identity.json"))?;
    file.write_all(&serde_json::to_vec(&ids)?)?;
    file.sync_all()?;
    let mut params: OverrideParams = serde_json::from_str("{}")?;
    params.mining_reserve = Some(MiningReserveAllocation {
        floor_script: pay_to_address_script(&Address::try_from(ids.floor.as_str())?),
        faucet_script: pay_to_address_script(&Address::try_from(ids.reserve.as_str())?),
    });
    let overrides = directory.join("reserve.json");
    std::fs::write(&overrides, serde_json::to_vec(&params)?)?;
    let rpc = format!("127.0.0.1:{}", port()?);
    let running = inprocess_node::InProcessNode::start_from_args(celeniumd_lib::args::Args {
        simnet: true, appdir: Some(directory.join("node").to_string_lossy().into()),
        override_params_file: Some(overrides.to_string_lossy().into()),
        rpclisten: Some(rpc.parse()?), listen: Some(format!("127.0.0.1:{}", port()?).parse()?),
        utxoindex: true, disable_upnp: true, disable_dns_seeding: true,
        outbound_target: 0, inbound_limit: 0, rpc_max_clients: 16, async_threads: 2,
        enable_unsynced_mining: true, block_template_cache_lifetime: Some(0),
        ram_scale: 0.1, no_log_files: true, log_level: "warn".into(), ..Default::default()
    })?;
    let outcome = async {
        let client = connect(&rpc).await?;
        ensure!(client.get_server_info().await?.network_id.network_type == NetworkType::Simnet);
        let (_shutdown, rx) = tokio::sync::watch::channel(false);
        let api = CeleniumApi::new(rpc.clone(), None, rx).await?;
        ensure!(api.funding_snapshot(&ids.operational).await?.mature_pradha == 0);
        emit(serde_json::json!({"ready":true,"rpc":rpc,"operational":ids.operational,"reserve":ids.reserve,"recipients":ids.recipients.iter().map(|x| &x.0).collect::<Vec<_>>(),"participants":ids.participants,"defaultStorageMass":true}));
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if send.send(line).is_err() { break }
            }
        });
        while let Some(line) = receive.recv().await {
            let command: serde_json::Value = serde_json::from_str(&line)?;
            let index = command["index"].as_u64().unwrap_or(0) as usize;
            match command["command"].as_str().unwrap_or("") {
                "mine" => { mine(&client, &Address::try_from(ids.miner.as_str())?, command["count"].as_u64().unwrap_or(15)).await?; emit(serde_json::json!({"mined":true})); }
                "spend" => {
                    let (address, secret) = &ids.recipients[index];
                    let tx = api.prepare_transfer(address, &ids.onward, 50_000_000, secret).await?;
                    let txid = api.submit_prepared_transfer(tx).await?;
                    emit(serde_json::json!({"spent":true,"txid":txid}));
                }
                "check" => {
                    let recipient = &ids.recipients[index].0;
                    let balance = client.get_balance_by_address(Address::try_from(recipient.as_str())?).await?;
                    let reserve = api.funding_snapshot(&ids.reserve).await?.mature_pradha;
                    let output = if let Some(txid) = command["txid"].as_str() { api.get_transaction_output_present(recipient, txid).await? } else { false };
                    emit(serde_json::json!({"balance":balance,"reserve":reserve,"outputPresent":output,"operational":api.funding_snapshot(&ids.operational).await?.mature_pradha}));
                }
                "stop" => break,
                _ => anyhow::bail!("unknown node command"),
            }
        }
        client.disconnect().await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    inprocess_node::shutdown_inprocess(running).await;
    outcome
}

async fn bridge(directory: &Path, rpc: &str, bind: &str) -> Result<()> {
    let ids = identity(directory)?;
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let api = CeleniumApi::new(rpc.into(), None, rx.clone()).await?;
    let state = Arc::new(Mutex::new(FaucetState::load(Path::new(celenium_stratum_bridge::FAUCET_LEDGER_PATH))?));
    state.lock().unwrap().set_funding_address(&ids.operational)?;
    prom::set_node_api(Arc::clone(&api));
    prom::set_faucet_state(Arc::clone(&state));
    prom::set_faucet_signer(None, Some(ids.operational.clone()));
    prom::set_faucet_reserve_signer(Arc::new(FaucetSigner::from_secret_hex_for_network(ids.reserve_secret, Prefix::Simnet)?), ids.reserve)?;
    treasury::start(api, state, None, Some(ids.operational), None, rx)?;
    emit(serde_json::json!({"bridgeReady":true,"pid":std::process::id()}));
    let result = prom::start_web_server_all(bind).await.map_err(|e| anyhow::anyhow!("{e}"));
    let _ = shutdown.send(true);
    result
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<()> {
    celenium_alloc::init_allocator_with_default_settings();
    let args: Vec<String> = std::env::args().collect();
    ensure!(args.len() >= 3, "usage: recovery_probe node DIR | bridge DIR RPC HTTP_BIND");
    let directory = Path::new(&args[2]).canonicalize()?;
    ensure!(directory.join(".recovery-sandbox").is_file(), "missing isolated sandbox marker");
    match args[1].as_str() {
        "node" => node(&directory).await,
        "bridge" => {
            ensure!(args.len() == 5);
            ensure!(std::env::current_dir()?.join(".recovery-sandbox").is_file(), "bridge cwd must be an isolated ledger directory");
            ensure!(args[3].starts_with("127.0.0.1:") && args[4].starts_with("127.0.0.1:"), "loopback required");
            bridge(&directory, &args[3], &args[4]).await
        }
        _ => anyhow::bail!("unknown mode"),
    }
}
