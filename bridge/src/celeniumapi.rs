use crate::log_colors::LogColors;
use crate::share_handler::CeleniumApiTrait;
use anyhow::{Context, Result};
use celenium_addresses::Address;
use celenium_consensus_core::block::Block;
use celenium_grpc_client::GrpcClient;
use celenium_notify::{listener::ListenerId, scope::NewBlockTemplateScope};
use celenium_rpc_core::notify::mode::NotificationMode;
use celenium_rpc_core::{
    GetBlockDagInfoRequest, GetBlockTemplateRequest, GetConnectedPeerInfoRequest, GetCurrentBlockColorRequest, GetFeeEstimateRequest,
    GetInfoRequest, GetServerInfoRequest, Notification, RpcHash, RpcRawBlock, SubmitBlockRequest, SubmitBlockResponse,
    api::rpc::RpcApi,
};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

const STRATUM_COINBASE_TAG_BYTES: &[u8] = b"RK-Stratum";
const MAX_COINBASE_TAG_SUFFIX_LEN: usize = 64;

fn mature_funding_total(
    params: &celenium_consensus_core::config::params::Params,
    virtual_daa_score: u64,
    source_script: &celenium_consensus_core::tx::ScriptPublicKey,
    entries: impl IntoIterator<Item = celenium_consensus_core::tx::UtxoEntry>,
) -> Result<u64> {
    entries.into_iter().try_fold(0u64, |total, entry| {
        if entry.script_public_key != *source_script || entry.covenant_id.is_some() {
            return Ok(total);
        }
        if entry.is_coinbase
            && !entry.block_daa_score.checked_add(params.coinbase_maturity()).is_some_and(|mature| mature <= virtual_daa_score)
        {
            return Ok(total);
        }
        total.checked_add(entry.amount).context("mature funding total overflows")
    })
}

fn funding_month(params: &celenium_consensus_core::config::params::Params, virtual_daa_score: u64) -> Result<u64> {
    anyhow::ensure!(
        params.deflationary_phase_daa_score == 0 && params.bps() == crate::emission_policy::BPS,
        "unsupported funding emission timing"
    );
    Ok(virtual_daa_score / crate::emission_policy::BPS / crate::emission_policy::SECONDS_PER_MONTH)
}

fn mempool_spent_outpoints(
    entries: impl IntoIterator<Item = celenium_rpc_core::RpcMempoolEntry>,
) -> HashSet<celenium_rpc_core::RpcTransactionOutpoint> {
    entries.into_iter().flat_map(|entry| entry.transaction.inputs).map(|input| input.previous_outpoint).collect()
}

fn unspent_utxos(
    entries: Vec<celenium_rpc_core::RpcUtxosByAddressesEntry>,
    spent: &HashSet<celenium_rpc_core::RpcTransactionOutpoint>,
) -> impl Iterator<Item = celenium_rpc_core::RpcUtxosByAddressesEntry> {
    entries.into_iter().filter(|entry| !spent.contains(&entry.outpoint))
}

fn apply_reconciliation_page(
    pending: &mut crate::treasury::PendingTransfer,
    changes: &celenium_rpc_core::GetVirtualChainFromBlockResponse,
) -> Result<bool> {
    let transaction_id = RpcHash::from_str(&pending.transaction_id).context("invalid journaled transaction id")?;
    if changes.accepted_transaction_ids.iter().any(|block| block.accepted_transaction_ids.contains(&transaction_id)) {
        return Ok(true);
    }
    if let Some(last) = changes.added_chain_block_hashes.last() {
        let next = last.to_string();
        if pending.anchor.as_deref() != Some(next.as_str()) {
            let covered: HashSet<_> = changes.accepted_transaction_ids.iter().map(|block| block.accepting_block_hash).collect();
            anyhow::ensure!(
                changes.added_chain_block_hashes.iter().all(|hash| covered.contains(hash)),
                "chain page is missing transaction acceptance data"
            );
            pending.anchor = Some(next);
        }
    }
    Ok(false)
}

fn require_consensus_utxos(server_version: &str) -> Result<()> {
    anyhow::ensure!(server_version.contains("utxo-consensus-v1"), "FundingUnavailable: node lacks consensus-backed UTXO queries");
    Ok(())
}

fn validate_journal_network(
    pending: &crate::treasury::PendingTransfer,
    node_network: celenium_consensus_core::network::NetworkId,
) -> Result<()> {
    let network = celenium_consensus_core::network::NetworkId::from_str(&pending.network).context("invalid journaled network")?;
    crate::transfer::network_params(network)?;
    anyhow::ensure!(node_network == network, "node network does not match journaled transfer");
    Ok(())
}

fn journaled_transaction(pending: &crate::treasury::PendingTransfer) -> Result<celenium_rpc_core::RpcTransaction> {
    use celenium_consensus_core::tx::{SignableTransaction, Transaction};
    let network = celenium_consensus_core::network::NetworkId::from_str(&pending.network).context("invalid journaled network")?;
    let params = crate::transfer::network_params(network)?;
    let source = Address::try_from(pending.source.as_str()).context("invalid journaled source")?;
    let destination = Address::try_from(pending.destination.as_str()).context("invalid journaled destination")?;
    anyhow::ensure!(source.prefix == params.prefix() && destination.prefix == params.prefix(), "journaled address network mismatch");
    anyhow::ensure!(source.version == celenium_addresses::Version::PubKey, "journaled source must be a public-key address");
    anyhow::ensure!(pending.amount_pradha > 0, "journaled amount must be positive");
    let transaction: celenium_rpc_core::RpcTransaction =
        serde_json::from_str(&pending.transaction_json).context("invalid journaled transaction JSON")?;
    let core = Transaction::try_from(transaction.clone()).context("invalid journaled transaction")?;
    anyhow::ensure!(core.id().to_string() == pending.transaction_id, "journaled transaction id does not match its contents");
    anyhow::ensure!(
        !core.inputs.is_empty()
            && core.inputs.len() <= 128.min(params.max_tx_inputs)
            && core.inputs.len() == pending.input_entries.len(),
        "journaled transaction input entries do not match its inputs"
    );
    anyhow::ensure!(
        core.subnetwork_id == celenium_consensus_core::subnets::SUBNETWORK_ID_NATIVE && core.payload.is_empty() && core.gas == 0,
        "journaled transaction is not a native transfer"
    );
    let source_script = celenium_txscript::pay_to_address_script(&source);
    let destination_script = celenium_txscript::pay_to_address_script(&destination);
    let payment = core.outputs.first().context("journaled transaction has no payment output")?;
    anyhow::ensure!(
        payment.value == pending.amount_pradha && payment.script_public_key == destination_script && payment.covenant.is_none(),
        "journaled transaction payment does not match its destination and amount"
    );
    anyhow::ensure!(
        core.outputs
            .iter()
            .skip(1)
            .all(|output| output.value > 0 && output.script_public_key == source_script && output.covenant.is_none()),
        "journaled transaction change does not return to its source"
    );
    let mut outpoints = HashSet::new();
    anyhow::ensure!(
        core.inputs.iter().all(|input| outpoints.insert(input.previous_outpoint)),
        "journaled transaction has duplicate inputs"
    );
    anyhow::ensure!(
        core.inputs
            .iter()
            .all(|input| input.signature_script.len() == 66 && input.signature_script[0] == 65 && input.signature_script[65] == 1),
        "journaled transaction must use complete SIGHASH_ALL signatures"
    );
    let inputs = pending.input_entries.iter().try_fold(0u64, |sum, entry| {
        anyhow::ensure!(
            entry.script_public_key == source_script && entry.covenant_id.is_none(),
            "journaled input does not belong to its source"
        );
        sum.checked_add(entry.amount).context("journaled input total overflows")
    })?;
    let outputs =
        core.outputs.iter().try_fold(0u64, |sum, output| sum.checked_add(output.value).context("journaled output total overflows"))?;
    anyhow::ensure!(
        inputs.checked_sub(outputs) == Some(pending.fee_pradha),
        "journaled transaction fee does not match its input entries"
    );
    let signed = SignableTransaction::with_entries(core, pending.input_entries.iter().cloned().map(Into::into).collect());
    celenium_consensus_core::sign::verify(&signed.as_verifiable()).context("invalid journaled transaction signatures")?;
    Ok(transaction)
}

fn sanitize_coinbase_tag_suffix(suffix: &str) -> Option<String> {
    let suffix = suffix.trim().trim_start_matches('/');
    if suffix.is_empty() {
        return None;
    }

    let mut out = String::with_capacity(suffix.len().min(MAX_COINBASE_TAG_SUFFIX_LEN));
    for ch in suffix.chars() {
        if out.len() >= MAX_COINBASE_TAG_SUFFIX_LEN {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else if ch.is_ascii_whitespace() {
            out.push('_');
        }
    }

    let out = out.trim_matches('_').to_string();
    if out.is_empty() { None } else { Some(out) }
}

fn build_coinbase_tag_bytes(suffix: Option<&str>) -> Vec<u8> {
    let mut tag = STRATUM_COINBASE_TAG_BYTES.to_vec();
    if let Some(suffix) = suffix.and_then(sanitize_coinbase_tag_suffix) {
        tag.push(b'/');
        tag.extend_from_slice(suffix.as_bytes());
    }
    tag
}

struct BlockSubmitGuard {
    ttl: Duration,
    max_entries: usize,
    entries: HashMap<String, Instant>,
    order: VecDeque<String>,
}

impl BlockSubmitGuard {
    fn new(ttl: Duration, max_entries: usize) -> Self {
        Self { ttl, max_entries, entries: HashMap::new(), order: VecDeque::new() }
    }

    fn prune(&mut self, now: Instant) {
        while let Some(front) = self.order.front() {
            let remove = match self.entries.get(front) {
                Some(ts) => now.duration_since(*ts) > self.ttl,
                None => true,
            };
            if remove {
                if let Some(key) = self.order.pop_front() {
                    self.entries.remove(&key);
                }
            } else {
                break;
            }
        }

        while self.entries.len() > self.max_entries {
            if let Some(key) = self.order.pop_front() {
                self.entries.remove(&key);
            } else {
                break;
            }
        }
    }

    fn try_mark(&mut self, hash: &str, now: Instant) -> bool {
        self.prune(now);
        if self.entries.contains_key(hash) {
            return false;
        }
        self.entries.insert(hash.to_string(), now);
        self.order.push_back(hash.to_string());
        true
    }

    fn remove(&mut self, hash: &str, now: Instant) {
        self.prune(now);
        self.entries.remove(hash);
    }
}

static BLOCK_SUBMIT_GUARD: Lazy<Mutex<BlockSubmitGuard>> =
    Lazy::new(|| Mutex::new(BlockSubmitGuard::new(Duration::from_secs(600), 50_000)));

#[derive(Clone, Debug, Default)]
pub struct NodeStatusSnapshot {
    pub last_updated: Option<std::time::Instant>,
    pub is_connected: bool,
    pub is_synced: Option<bool>,
    pub network_id: Option<String>,
    pub server_version: Option<String>,
    pub virtual_daa_score: Option<u64>,
    pub block_count: Option<u64>,
    pub header_count: Option<u64>,
    pub difficulty: Option<f64>,
    pub tip_hash: Option<String>,
    pub peers: Option<usize>,
    pub mempool_size: Option<u64>,
}

pub static NODE_STATUS: Lazy<Mutex<NodeStatusSnapshot>> = Lazy::new(|| Mutex::new(NodeStatusSnapshot::default()));

/// Celenium API client wrapper using RPC client
/// Both use gRPC under the hood, but through an RPC client wrapper abstraction
pub struct CeleniumApi {
    client: Arc<GrpcClient>,
    notification_rx: Arc<Mutex<Option<mpsc::UnboundedReceiver<Notification>>>>,
    connected: Arc<Mutex<bool>>,
    coinbase_tag: Vec<u8>,
}

impl CeleniumApi {
    /// Create a new Celenium API client
    pub async fn new(
        address: String,
        coinbase_tag_suffix: Option<String>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Arc<Self>> {
        info!("Connecting to Celenium node at {}", address);

        // GrpcClient requires explicit "grpc://" prefix for connection
        // Always add it if not present (avoids unnecessary connection failure)
        let grpc_address = if address.starts_with("grpc://") { address.clone() } else { format!("grpc://{}", address) };

        // Log connection attempt (detailed logs moved to debug)
        debug!("{} {}", LogColors::api("[API]"), LogColors::label("Establishing RPC connection to Celenium node:"));
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Address:"), &grpc_address);
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Protocol:"), "gRPC (via RPC client wrapper)");

        let mut attempt: u64 = 0;
        let mut backoff_ms: u64 = 250;

        let client = loop {
            #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(COUNTER)")]
            {
                attempt += 1;
            }
            let connect_fut = GrpcClient::connect_with_args(
                NotificationMode::Direct,
                grpc_address.clone(),
                None,
                true,
                None,
                false,
                Some(500_000),
                Default::default(),
            );

            let res = tokio::select! {
                _ = shutdown_rx.wait_for(|v| *v) => {
                    return Err(anyhow::anyhow!("shutdown requested"));
                }
                res = connect_fut => res,
            };

            match res {
                Ok(client) => break Arc::new(client),
                Err(e) => {
                    let backoff = Duration::from_millis(backoff_ms);
                    warn!(
                        "failed to connect to celenium node at {} (attempt {}): {}, retrying in {:.2}s",
                        grpc_address,
                        attempt,
                        e,
                        backoff.as_secs_f64()
                    );

                    tokio::select! {
                        _ = shutdown_rx.wait_for(|v| *v) => {
                            return Err(anyhow::anyhow!("shutdown requested"));
                        }
                        _ = sleep(backoff) => {}
                    }

                    backoff_ms = (backoff_ms.saturating_mul(2)).min(5_000);
                }
            }
        };

        // Log successful connection (detailed logs moved to debug)
        debug!("{} {}", LogColors::api("[API]"), LogColors::block("RPC Connection Established Successfully"));
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Connected to:"), &grpc_address);
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Connection Type:"), "gRPC (via RPC client wrapper)");

        // Start the client (no notify needed for Direct mode)
        client.start(None).await;

        // Subscribe to block template notifications
        // Some nodes may take time to accept notification subscriptions; retry until it succeeds.
        // This retry logic with exponential backoff handles transient failures where nodes are not
        // immediately ready to accept subscriptions after connection, preventing tight-looping and log spam.
        let mut attempt: u64 = 0;
        let mut backoff_ms: u64 = 250;
        loop {
            #[allow(clippy::arithmetic_side_effects, reason = "ARITH-SAFETY(COUNTER)")]
            {
                attempt += 1;
            }
            let notify_fut = client.start_notify(ListenerId::default(), NewBlockTemplateScope {}.into());

            let res = tokio::select! {
                _ = shutdown_rx.wait_for(|v| *v) => {
                    return Err(anyhow::anyhow!("shutdown requested"));
                }
                res = notify_fut => res,
            };

            match res {
                Ok(_) => break,
                Err(e) => {
                    let backoff = Duration::from_millis(backoff_ms);
                    warn!(
                        "failed to subscribe to block template notifications (attempt {}): {}, retrying in {:.2}s",
                        attempt,
                        e,
                        backoff.as_secs_f64()
                    );

                    tokio::select! {
                        _ = shutdown_rx.wait_for(|v| *v) => {
                            return Err(anyhow::anyhow!("shutdown requested"));
                        }
                        _ = sleep(backoff) => {}
                    }
                    backoff_ms = (backoff_ms.saturating_mul(2)).min(5_000);
                }
            }
        }

        // Start receiving notifications
        let notification_rx = {
            let receiver = client.notification_channel_receiver();
            // Convert async_channel::Receiver to tokio::sync::mpsc::UnboundedReceiver
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            let receiver_clone = receiver.clone();
            tokio::spawn(async move {
                while let Ok(notification) = receiver_clone.recv().await {
                    let _ = tx.send(notification);
                }
            });
            Arc::new(Mutex::new(Some(rx)))
        };

        let coinbase_tag = build_coinbase_tag_bytes(coinbase_tag_suffix.as_deref());
        let api = Arc::new(Self { client, notification_rx, connected: Arc::new(Mutex::new(true)), coinbase_tag });

        // Start network stats thread
        let api_clone = Arc::clone(&api);
        tokio::spawn(async move {
            api_clone.start_stats_thread().await;
        });

        // Start node status polling thread (for console status display)
        let api_clone = Arc::clone(&api);
        tokio::spawn(async move {
            api_clone.start_node_status_thread().await;
        });

        Ok(api)
    }

    /// Start network stats thread
    /// Fetches network stats every 30 seconds and records them in Prometheus
    async fn start_stats_thread(self: Arc<Self>) {
        use crate::prom::record_network_stats;
        use celenium_rpc_core::{EstimateNetworkHashesPerSecondRequest, GetBlockDagInfoRequest};

        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;

            // Get block DAG info
            // GetBlockDagInfoRequest is a unit struct, construct directly
            let dag_response = match self.client.get_block_dag_info_call(None, GetBlockDagInfoRequest {}).await {
                Ok(r) => r,
                Err(e) => {
                    warn!("failed to get network hashrate from celenium, prom stats will be out of date: {}", e);
                    continue;
                }
            };

            // Get tip hash (first one)
            // tip_hashes is Vec<Hash> in the response (already parsed)
            let tip_hash = match dag_response.tip_hashes.first() {
                Some(hash) => Some(*hash), // Clone the Hash
                None => {
                    warn!("no tip hashes available for network hashrate estimation");
                    continue;
                }
            };

            // Estimate network hashes per second
            // new(window_size: u32, start_hash: Option<RpcHash>)
            // RpcHash is the same as Hash, so we can use tip_hash directly
            let hashrate_response = match self
                .client
                .estimate_network_hashes_per_second_call(None, EstimateNetworkHashesPerSecondRequest::new(1000, tip_hash))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    warn!("failed to get network hashrate from celenium, prom stats will be out of date: {}", e);
                    continue;
                }
            };

            // Record network stats
            record_network_stats(hashrate_response.network_hashes_per_second, dag_response.block_count, dag_response.difficulty);
        }
    }

    async fn start_node_status_thread(self: Arc<Self>) {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        loop {
            interval.tick().await;

            let connected = self.client.is_connected();

            let server_info_fut = self.client.get_server_info_call(None, GetServerInfoRequest {});
            let dag_info_fut = self.client.get_block_dag_info_call(None, GetBlockDagInfoRequest {});
            let peers_fut = self.client.get_connected_peer_info_call(None, GetConnectedPeerInfoRequest {});
            let info_fut = self.client.get_info_call(None, GetInfoRequest {});

            let (server_info, dag_info, peers_info, info_resp) = tokio::join!(server_info_fut, dag_info_fut, peers_fut, info_fut);

            let mut snapshot = NODE_STATUS.lock();
            snapshot.last_updated = Some(std::time::Instant::now());
            snapshot.is_connected = connected;

            if let Ok(server_info) = server_info {
                snapshot.is_synced = Some(server_info.is_synced);
                snapshot.network_id = Some(format!("{:?}", server_info.network_id));
                snapshot.server_version = Some(server_info.server_version);
                snapshot.virtual_daa_score = Some(server_info.virtual_daa_score);
            }

            if let Ok(dag) = dag_info {
                snapshot.block_count = Some(dag.block_count);
                snapshot.header_count = Some(dag.header_count);
                snapshot.difficulty = Some(dag.difficulty);
                snapshot.tip_hash = dag.tip_hashes.first().map(|h| format!("{}", h));
                if snapshot.virtual_daa_score.is_none() {
                    snapshot.virtual_daa_score = Some(dag.virtual_daa_score);
                }
                if snapshot.network_id.is_none() {
                    snapshot.network_id = Some(format!("{:?}", dag.network));
                }
            }

            if let Ok(peers) = peers_info {
                snapshot.peers = Some(peers.peer_info.len());
            }

            if let Ok(info) = info_resp {
                snapshot.mempool_size = Some(info.mempool_size);
                if snapshot.server_version.is_none() {
                    snapshot.server_version = Some(info.server_version);
                }
            }
        }
    }

    /// Submit a block
    pub async fn submit_block(&self, block: Block) -> Result<SubmitBlockResponse> {
        // Use celenium_consensus_core::hashing::header::hash() for block hash calculation
        // In Celenium, the block hash is the header hash (transactions are represented by hash_merkle_root in header)
        use celenium_consensus_core::hashing::header;
        let block_hash = header::hash(&block.header).to_string();
        let blue_score = block.header.blue_score;
        let timestamp = block.header.timestamp;
        let nonce = block.header.nonce;

        {
            let now = Instant::now();
            let mut guard = BLOCK_SUBMIT_GUARD.lock();
            if !guard.try_mark(&block_hash, now) {
                return Err(anyhow::anyhow!("ErrDuplicateBlock: block already submitted"));
            }
        }

        debug!(
            "{} {}",
            LogColors::api("[API]"),
            LogColors::api(&format!("===== ATTEMPTING BLOCK SUBMISSION TO CELENIUM NODE ===== Hash: {}", block_hash))
        );
        debug!("{} {}", LogColors::api("[API]"), LogColors::label("Block Details:"));
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Hash:"), block_hash);
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Blue Score:"), blue_score);
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Timestamp:"), timestamp);
        debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Nonce:"), format!("{:x} ({})", nonce, nonce));
        debug!("{} {}", LogColors::api("[API]"), "Converting block to RPC format and sending to node...");

        // Convert Block to RpcRawBlock (use reference)
        let rpc_block: RpcRawBlock = (&block).into();

        // Submit block (don't allow non-DAA blocks)
        debug!("{} {}", LogColors::api("[API]"), "Calling submit_block via RPC client...");
        let result =
            self.client.submit_block_call(None, SubmitBlockRequest::new(rpc_block, false)).await.context("Failed to submit block");

        if let Err(e) = &result {
            let error_str = e.to_string();
            let is_duplicate = error_str.contains("ErrDuplicateBlock") || error_str.contains("duplicate");
            if !is_duplicate {
                let now = Instant::now();
                let mut guard = BLOCK_SUBMIT_GUARD.lock();
                guard.remove(&block_hash, now);
            }
        }

        match &result {
            Ok(response) => {
                // IMPORTANT: The RPC call can succeed while the node still rejects the block.
                // Only treat SubmitBlockReport::Success as accepted.
                if !response.report.is_success() {
                    let now = Instant::now();
                    let mut guard = BLOCK_SUBMIT_GUARD.lock();
                    guard.remove(&block_hash, now);

                    warn!(
                        "{} {}",
                        LogColors::api("[API]"),
                        LogColors::validation(&format!("===== BLOCK REJECTED BY CELENIUM NODE ===== Hash: {}", block_hash))
                    );
                    warn!(
                        "{} {} {}",
                        LogColors::api("[API]"),
                        LogColors::label("REJECTION REASON:"),
                        format!("{:?}", response.report)
                    );
                    warn!(
                        "{} {} {}",
                        LogColors::api("[API]"),
                        LogColors::label("  - Blue Score:"),
                        format!("{}, Timestamp: {}, Nonce: {:x}", blue_score, timestamp, nonce)
                    );
                    return Err(anyhow::anyhow!("Block rejected by node: {:?}", response.report));
                }

                // Keep block accepted message at info (important operational event)
                info!(
                    "{} {}",
                    LogColors::api("[API]"),
                    LogColors::block(&format!("===== BLOCK ACCEPTED BY CELENIUM NODE ===== Hash: {}", block_hash))
                );
                // Detailed acceptance logs moved to debug
                debug!(
                    "{} {} {}",
                    LogColors::api("[API]"),
                    LogColors::label("ACCEPTANCE REASON:"),
                    "Block passed all node validation checks"
                );
                debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Block structure:"), "VALID");
                debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Block header:"), "VALID");
                debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Transactions:"), "VALID");
                debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - DAA validation:"), "PASSED");
                debug!("{} {} {}", LogColors::api("[API]"), LogColors::label("  - Node Response:"), format!("{:?}", response));
                debug!(
                    "{} {} {}",
                    LogColors::api("[API]"),
                    LogColors::label("  - Blue Score:"),
                    format!("{}, Timestamp: {}, Nonce: {:x}", blue_score, timestamp, nonce)
                );

                // Optional: Check if block appears in tip hashes (verifies propagation)
                // This is informational only - block may still propagate even if not immediately in tips
                let client_clone = Arc::clone(&self.client);
                let block_hash_clone = block_hash.clone();
                let block_hash_for_check = header::hash(&block.header); // Use the actual Hash type
                tokio::spawn(async move {
                    // Wait a bit for block to be processed and potentially added to DAG
                    tokio::time::sleep(Duration::from_secs(2)).await;

                    // Check if block appears in tip hashes
                    if let Ok(dag_response) = client_clone.get_block_dag_info_call(None, GetBlockDagInfoRequest {}).await {
                        // Check if our block hash is in tip hashes
                        let in_tips = dag_response.tip_hashes.contains(&block_hash_for_check);

                        if in_tips {
                            info!(
                                "{} {} {}",
                                LogColors::api("[API]"),
                                LogColors::block("Block appears in tip hashes (good sign for propagation)"),
                                format!("Hash: {}", block_hash_clone)
                            );
                        } else {
                            // This is not necessarily bad - block may still propagate or be in a side chain
                            info!(
                                "{} {} {}",
                                LogColors::api("[API]"),
                                LogColors::label("Block not yet in tip hashes (may still propagate)"),
                                format!("Hash: {}", block_hash_clone)
                            );
                            info!(
                                "{} {} {}",
                                LogColors::api("[API]"),
                                LogColors::label("  - Note:"),
                                "Block may be in a side chain or still propagating"
                            );
                            info!(
                                "{} {} {}",
                                LogColors::api("[API]"),
                                LogColors::label("  - Tip hashes count:"),
                                dag_response.tip_hashes.len()
                            );
                        }
                    }
                });
            }
            Err(e) => {
                let error_str = e.to_string();
                if error_str.contains("ErrDuplicateBlock") || error_str.contains("duplicate") {
                    warn!(
                        "{} {}",
                        LogColors::api("[API]"),
                        LogColors::validation(&format!("===== BLOCK REJECTED BY CELENIUM NODE: STALE ===== Hash: {}", block_hash))
                    );
                    warn!(
                        "{} {} {}",
                        LogColors::api("[API]"),
                        LogColors::label("REJECTION REASON:"),
                        "Block already exists in the network"
                    );
                    warn!("{} {}", LogColors::api("[API]"), LogColors::label("  - Block was previously submitted and accepted"));
                    warn!("{} {}", LogColors::api("[API]"), LogColors::label("  - This is a duplicate/stale block submission"));
                    warn!("{} {} {}", LogColors::api("[API]"), LogColors::error("  - Error:"), error_str);
                    warn!(
                        "{} {} {}",
                        LogColors::api("[API]"),
                        LogColors::label("  - Blue Score:"),
                        format!("{}, Timestamp: {}, Nonce: {:x}", blue_score, timestamp, nonce)
                    );
                } else {
                    error!(
                        "{} {}",
                        LogColors::api("[API]"),
                        LogColors::error(&format!("===== BLOCK REJECTED BY CELENIUM NODE: INVALID ===== Hash: {}", block_hash))
                    );
                    error!("{} {} {}", LogColors::api("[API]"), LogColors::label("REJECTION REASON:"), "Block failed node validation");
                    error!("{} {}", LogColors::api("[API]"), LogColors::label("  - Possible validation failures:"));
                    error!("{} {}", LogColors::api("[API]"), "    * Invalid block structure or format");
                    error!("{} {}", LogColors::api("[API]"), "    * Block header validation failed");
                    error!("{} {}", LogColors::api("[API]"), "    * Transaction validation failed");
                    error!("{} {}", LogColors::api("[API]"), "    * DAA (Difficulty Adjustment Algorithm) validation failed");
                    error!("{} {}", LogColors::api("[API]"), "    * Block does not meet network consensus rules");
                    error!("{} {} {}", LogColors::api("[API]"), LogColors::error("  - Error from node:"), error_str);
                    error!(
                        "{} {} {}",
                        LogColors::api("[API]"),
                        LogColors::label("  - Blue Score:"),
                        format!("{}, Timestamp: {}, Nonce: {:x}", blue_score, timestamp, nonce)
                    );
                }
            }
        }

        result
    }

    /// Wait for node to sync
    async fn wait_for_sync(&self) -> Result<()> {
        loop {
            match self.client.get_sync_status().await {
                Ok(is_synced) => {
                    if is_synced {
                        break;
                    }
                }
                Err(e) => {
                    debug!("failed to get sync status: {}, retrying...", e);
                }
            }

            sleep(Duration::from_secs(10)).await;
        }

        Ok(())
    }

    pub async fn wait_for_sync_with_shutdown(&self, mut shutdown_rx: watch::Receiver<bool>, skip_sync_wait: bool) -> Result<()> {
        debug!("checking celeniumd sync state");
        if skip_sync_wait {
            // Solo/fresh chains never report is_synced (sync rate 0 with no
            // peers); mining rules already allow mining when the finality
            // point is recent, so honoring the operator's flag here is safe.
            tracing::warn!("--skip-sync-wait set: skipping the sync gate");
            return Ok(());
        }
        loop {
            let sync_fut = self.client.get_sync_status();
            let sync_res = tokio::select! {
                _ = shutdown_rx.wait_for(|v| *v) => {
                    return Err(anyhow::anyhow!("shutdown requested"));
                }
                res = sync_fut => res,
            };

            match sync_res {
                Ok(is_synced) => {
                    if is_synced {
                        debug!("celeniumd synced, starting server");
                        break;
                    }
                }
                Err(e) => {
                    warn!("failed to get sync status: {}, retrying...", e);
                }
            }

            warn!("Celenium is not synced, waiting for sync before starting bridge");

            tokio::select! {
                _ = shutdown_rx.wait_for(|v| *v) => {
                    return Err(anyhow::anyhow!("shutdown requested"));
                }
                _ = sleep(Duration::from_secs(10)) => {}
            }
        }

        Ok(())
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        *self.connected.lock()
    }

    /// Get block template for a client
    pub async fn get_block_template(&self, wallet_addr: &str, _remote_app: &str, _canxium_addr: &str) -> Result<Block> {
        // Retry up to 3 times if we get "Odd number of digits" error
        // This error can occur if the block template has malformed hash fields
        let max_retries = 3;
        let mut last_error = None;

        for attempt in 0..max_retries {
            // Parse wallet address each time (in case Address doesn't implement Clone)
            let address =
                Address::try_from(wallet_addr).map_err(|e| anyhow::anyhow!("Could not decode address {}: {}", wallet_addr, e))?;

            // Request block template using RPC client wrapper
            let response = match self
                .client
                .get_block_template_call(None, GetBlockTemplateRequest::new(address, self.coinbase_tag.clone()))
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt < max_retries - 1 {
                        #[allow(clippy::arithmetic_side_effects, reason = "attempt is in 0..3, so attempt + 1 <= 3.")]
                        {
                            warn!("Failed to get block template (attempt {}/{}): {}, retrying...", attempt + 1, max_retries, e);
                        }
                        #[allow(
                            clippy::arithmetic_side_effects,
                            reason = "attempt is in 0..3, so the backoff is at most 300 milliseconds."
                        )]
                        {
                            sleep(Duration::from_millis(100 * (attempt + 1) as u64)).await;
                        }
                        continue;
                    }
                    return Err(anyhow::anyhow!("Failed to get block template after {} attempts: {}", max_retries, e));
                }
            };

            // Get RPC block from response
            let rpc_block = response.block;

            // Convert RpcRawBlock to Block
            // The RpcRawBlock contains the block data that we need to convert
            // The "Odd number of digits" error can occur here if hash fields have malformed hex strings
            match Block::try_from(rpc_block) {
                Ok(block) => {
                    // Validate that we can serialize the block header
                    // This catches "Odd number of digits" errors early
                    // Convert error to String immediately to avoid Send issues
                    let serialize_result = crate::hasher::serialize_block_header(&block).map_err(|e| e.to_string());

                    match serialize_result {
                        Ok(_) => {
                            return Ok(block);
                        }
                        Err(error_str) => {
                            if error_str.contains("Odd number of digits") {
                                last_error = Some(format!("Block has malformed hash field: {}", error_str));
                                if attempt < max_retries - 1 {
                                    #[allow(clippy::arithmetic_side_effects, reason = "attempt is in 0..3, so attempt + 1 <= 3.")]
                                    {
                                        warn!(
                                            "Block template has malformed hash field (attempt {}/{}), retrying...",
                                            attempt + 1,
                                            max_retries
                                        );
                                    }
                                    #[allow(
                                        clippy::arithmetic_side_effects,
                                        reason = "attempt is in 0..3, so the backoff is at most 300 milliseconds."
                                    )]
                                    {
                                        sleep(Duration::from_millis(100 * (attempt + 1) as u64)).await;
                                    }
                                    continue;
                                }
                            }
                            // If it's a different error, return it
                            return Err(anyhow::anyhow!("Failed to serialize block header: {}", error_str));
                        }
                    }
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    last_error = Some(error_str.clone());
                    if error_str.contains("Odd number of digits") && attempt < max_retries - 1 {
                        #[allow(clippy::arithmetic_side_effects, reason = "attempt is in 0..3, so attempt + 1 <= 3.")]
                        {
                            warn!(
                                "Block conversion failed with 'Odd number of digits' error (attempt {}/{}), retrying...",
                                attempt + 1,
                                max_retries
                            );
                        }
                        #[allow(
                            clippy::arithmetic_side_effects,
                            reason = "attempt is in 0..3, so the backoff is at most 300 milliseconds."
                        )]
                        {
                            sleep(Duration::from_millis(100 * (attempt + 1) as u64)).await;
                        }
                        continue;
                    }
                    // If the error contains "Odd number of digits", provide more context
                    if error_str.contains("Odd number of digits") {
                        return Err(anyhow::anyhow!(
                            "Failed to convert RPC block to Block after {} attempts: {} - This usually indicates a malformed hash field in the block template from the Celenium node. The block may have a hash with an odd-length hex string.",
                            max_retries,
                            error_str
                        ));
                    } else {
                        return Err(anyhow::anyhow!("Failed to convert RPC block to Block: {}", error_str));
                    }
                }
            }
        }

        // Should never reach here, but handle it just in case
        Err(anyhow::anyhow!("Failed to get valid block template after {} attempts: {:?}", max_retries, last_error))
    }

    /// Get balances by addresses (for Prometheus metrics)
    pub async fn get_balances_by_addresses(&self, addresses: &[String]) -> Result<Vec<(String, u64)>> {
        let parsed_addresses: Result<Vec<Address>, _> = addresses.iter().map(|addr| Address::try_from(addr.as_str())).collect();

        let addresses = parsed_addresses.map_err(|e| anyhow::anyhow!("Failed to parse addresses: {:?}", e))?;

        let utxos = self
            .client
            .get_utxos_by_addresses_call(None, celenium_rpc_core::GetUtxosByAddressesRequest::new(addresses))
            .await
            .context("Failed to get UTXOs by addresses")?;

        // Calculate balances from UTXOs
        // Group entries by address
        let mut balance_map: HashMap<String, u64> = HashMap::new();
        for entry in utxos.entries {
            if let Some(address) = entry.address {
                let addr_str = address.to_string();
                let amount = entry.utxo_entry.amount;
                let balance = balance_map.entry(addr_str).or_insert(0);
                *balance = balance.checked_add(amount).context("address balance exceeds u64::MAX")?;
            }
        }
        let balances: Vec<(String, u64)> = balance_map.into_iter().collect();

        Ok(balances)
    }

    async fn pending_spent_outpoints(&self) -> Result<HashSet<celenium_rpc_core::RpcTransactionOutpoint>> {
        let response = self
            .client
            .get_mempool_entries_call(
                None,
                celenium_rpc_core::GetMempoolEntriesRequest { include_orphan_pool: true, filter_transaction_pool: false },
            )
            .await
            .context("get_mempool_entries failed")?;
        Ok(mempool_spent_outpoints(response.mempool_entries))
    }

    async fn unspent_address_entries(&self, source: &Address) -> Result<Vec<celenium_rpc_core::RpcUtxosByAddressesEntry>> {
        let mut spent = self.pending_spent_outpoints().await?;
        let utxos = self
            .client
            .get_utxos_by_addresses_call(None, celenium_rpc_core::GetUtxosByAddressesRequest::new(vec![source.clone()]))
            .await
            .context("get_utxos_by_addresses failed")?;
        // Keep reservations for transactions that confirm while the UTXO query is in flight.
        spent.extend(self.pending_spent_outpoints().await?);
        Ok(unspent_utxos(utxos.entries, &spent).collect())
    }

    pub async fn funding_snapshot(&self, address: &str) -> Result<crate::reservoir_funds::FundingSnapshot> {
        let source = Address::try_from(address).context("invalid funding address")?;
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        require_consensus_utxos(&info.server_version)?;
        let params = crate::transfer::network_params(info.network_id)?;
        anyhow::ensure!(source.prefix == params.prefix(), "address network does not match node network");
        let month = funding_month(&params, info.virtual_daa_score)?;
        let entries = self.unspent_address_entries(&source).await?.into_iter().map(|entry| entry.utxo_entry.into());
        let mature_pradha =
            mature_funding_total(&params, info.virtual_daa_score, &celenium_txscript::pay_to_address_script(&source), entries)?;
        Ok(crate::reservoir_funds::FundingSnapshot { mature_pradha, observed_at: crate::faucet::FaucetState::now_secs(), month })
    }

    pub async fn faucet_funding_snapshot(
        &self,
        operational: &str,
        reserve: Option<&str>,
    ) -> Result<crate::reservoir_funds::FaucetFundingSnapshot> {
        let prefix = crate::faucet_funding::validate_addresses(operational, reserve)?;
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        require_consensus_utxos(&info.server_version)?;
        let params = crate::transfer::network_params(info.network_id)?;
        anyhow::ensure!(prefix == params.prefix(), "address network does not match node network");
        let month = funding_month(&params, info.virtual_daa_score)?;
        let source = Address::try_from(operational)?;
        let backup = reserve.map(Address::try_from).transpose()?;
        let mut addresses = vec![source.clone()];
        addresses.extend(backup.iter().cloned());
        let mut spent = self.pending_spent_outpoints().await?;
        let response = self
            .client
            .get_utxos_by_addresses_call(None, celenium_rpc_core::GetUtxosByAddressesRequest::new(addresses))
            .await
            .context("get_utxos_by_addresses failed")?;
        spent.extend(self.pending_spent_outpoints().await?);
        let after = self.client.get_server_info_call(None, GetServerInfoRequest {}).await?;
        anyhow::ensure!(info.network_id == after.network_id, "node network changed during funding query");
        let mut seen = HashSet::new();
        let entries: Vec<celenium_consensus_core::tx::UtxoEntry> = unspent_utxos(response.entries, &spent)
            .filter(|entry| seen.insert(entry.outpoint.clone()))
            .map(|entry| entry.utxo_entry.into())
            .collect();
        let observed_at = crate::faucet::FaucetState::now_secs();
        let snapshot = |address: &Address| -> Result<crate::reservoir_funds::FundingSnapshot> {
            let mature_pradha = mature_funding_total(
                &params,
                info.virtual_daa_score,
                &celenium_txscript::pay_to_address_script(address),
                entries.iter().cloned(),
            )?;
            Ok(crate::reservoir_funds::FundingSnapshot { mature_pradha, observed_at, month })
        };
        Ok(crate::reservoir_funds::FaucetFundingSnapshot {
            operational: snapshot(&source)?,
            reserve: backup.as_ref().map(snapshot).transpose()?,
            reserve_configured: reserve.is_some(),
        })
    }

    pub async fn get_transaction_output_present(&self, address: &str, txid: &str) -> Result<bool> {
        RpcHash::from_str(txid).context("invalid transaction id")?;
        let source = Address::try_from(address).context("invalid transaction output address")?;
        let utxos = self
            .client
            .get_utxos_by_addresses_call(None, celenium_rpc_core::GetUtxosByAddressesRequest::new(vec![source]))
            .await
            .context("get_utxos_by_addresses failed")?;
        Ok(utxos.entries.iter().any(|entry| entry.outpoint.transaction_id.to_string() == txid))
    }

    pub async fn transfer_anchor(&self) -> Result<String> {
        Ok(self.client.get_block_dag_info().await.context("cannot read transfer anchor")?.sink.to_string())
    }

    pub async fn journaled_transfer_accepted(&self, pending: &mut crate::treasury::PendingTransfer) -> Result<bool> {
        let transaction = journaled_transaction(pending)?;
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        validate_journal_network(pending, info.network_id)?;
        if let Some(anchor) = &pending.anchor {
            let changes = self
                .client
                .get_virtual_chain_from_block(RpcHash::from_str(anchor)?, true, Some(10))
                .await
                .context("cannot reconcile journaled transaction from its chain anchor")?;
            return apply_reconciliation_page(pending, &changes);
        }
        let destination = Address::try_from(pending.destination.as_str())?;
        let utxos = self
            .client
            .get_utxos_by_addresses_call(None, celenium_rpc_core::GetUtxosByAddressesRequest::new(vec![destination]))
            .await
            .context("cannot reconcile journaled payment output")?;
        let payment = &transaction.outputs[0];
        Ok(utxos.entries.iter().any(|entry| {
            entry.outpoint.transaction_id.to_string() == pending.transaction_id
                && entry.outpoint.index == 0
                && entry.utxo_entry.amount == payment.value
                && entry.utxo_entry.script_public_key == payment.script_public_key
                && entry.utxo_entry.covenant_id.is_none()
        }))
    }

    pub async fn submit_journaled_transfer(&self, pending: &crate::treasury::PendingTransfer) -> Result<String> {
        let transaction = journaled_transaction(pending)?;
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        validate_journal_network(pending, info.network_id)?;
        let submitted = self
            .client
            .submit_transaction_call(None, celenium_rpc_core::SubmitTransactionRequest::new(transaction, false))
            .await
            .context("submit_transaction rejected")?
            .transaction_id
            .to_string();
        anyhow::ensure!(submitted == pending.transaction_id, "node returned a different transaction id");
        Ok(submitted)
    }

    pub async fn send_coins(
        &self,
        from_address: &str,
        to_address: &str,
        amount_pradha: u64,
        fee_pradha: u64,
        private_key_hex: &str,
    ) -> Result<String> {
        let prepared =
            self.prepare_transfer_inner(from_address, to_address, amount_pradha, Some(fee_pradha), private_key_hex, None).await?;
        self.submit_prepared_transfer(prepared).await
    }

    /// Signs an exact quote without broadcasting, so the faucet can check its fee budget first.
    pub async fn prepare_transfer(
        &self,
        from_address: &str,
        to_address: &str,
        amount_pradha: u64,
        private_key_hex: &str,
    ) -> Result<crate::transfer::PreparedTransfer> {
        self.prepare_transfer_inner(from_address, to_address, amount_pradha, None, private_key_hex, None).await
    }

    pub async fn prepare_surplus_transfer(
        &self,
        from_address: &str,
        to_address: &str,
        max_amount_pradha: u64,
        private_key_hex: &str,
    ) -> Result<crate::transfer::PreparedTransfer> {
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        require_consensus_utxos(&info.server_version)?;
        let params = crate::transfer::network_params(info.network_id)?;
        let source = Address::try_from(from_address).context("invalid source address")?;
        let destination = Address::try_from(to_address).context("invalid destination address")?;
        let secret = hex::decode(private_key_hex.trim().trim_start_matches("0x")).context("secret is not valid hex")?;
        let secret: [u8; 32] = secret.try_into().map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;
        crate::transfer::validate_request(&params, &source, &destination, max_amount_pradha, &secret)?;
        let feerate = self.transfer_feerate().await?;
        let inputs = self
            .unspent_address_entries(&source)
            .await?
            .into_iter()
            .map(|entry| (entry.outpoint.into(), entry.utxo_entry.into()))
            .collect();
        let signed = crate::transfer::build_surplus_transfer(
            &params,
            info.virtual_daa_score,
            &source,
            &destination,
            max_amount_pradha,
            feerate,
            &secret,
            inputs,
        )?;
        crate::transfer::PreparedTransfer::new(signed, &params)
    }

    /// Local simnet experiments must select the same policy as their isolated node.
    pub async fn prepare_simnet_transfer(
        &self,
        from_address: &str,
        to_address: &str,
        amount_pradha: u64,
        private_key_hex: &str,
        storage_mass_amount_floor: u64,
    ) -> Result<crate::transfer::PreparedTransfer> {
        self.prepare_transfer_inner(from_address, to_address, amount_pradha, None, private_key_hex, Some(storage_mass_amount_floor))
            .await
    }

    async fn transfer_feerate(&self) -> Result<f64> {
        let response = self.client.get_fee_estimate_call(None, GetFeeEstimateRequest {}).await.context("get_fee_estimate failed")?;
        let rate = response.estimate.low_buckets.first().map(|bucket| bucket.feerate).unwrap_or(crate::transfer::FALLBACK_FEERATE);
        anyhow::ensure!(rate.is_finite() && rate >= 0.0, "node returned an invalid fee rate");
        Ok(rate.max(crate::transfer::FALLBACK_FEERATE))
    }

    async fn prepare_transfer_inner(
        &self,
        from_address: &str,
        to_address: &str,
        amount_pradha: u64,
        fee_pradha: Option<u64>,
        private_key_hex: &str,
        simnet_floor: Option<u64>,
    ) -> Result<crate::transfer::PreparedTransfer> {
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        let mut params = crate::transfer::network_params(info.network_id)?;
        if let Some(floor) = simnet_floor {
            anyhow::ensure!(
                params.net.network_type == celenium_consensus_core::network::NetworkType::Simnet,
                "experimental transfer policy is restricted to simnet"
            );
            params.storage_mass_amount_floor = floor;
        }
        let source = Address::try_from(from_address).context("invalid source address")?;
        let destination = Address::try_from(to_address).context("invalid destination address")?;
        let secret = hex::decode(private_key_hex.trim().trim_start_matches("0x")).context("secret is not valid hex")?;
        let secret: [u8; 32] = secret.try_into().map_err(|_| anyhow::anyhow!("secret must be 32 bytes"))?;
        crate::transfer::validate_request(&params, &source, &destination, amount_pradha, &secret)?;
        let feerate = self.transfer_feerate().await?;
        let inputs =
            self.unspent_address_entries(&source).await?.into_iter().map(|u| (u.outpoint.into(), u.utxo_entry.into())).collect();
        let signed = crate::transfer::build_transfer(
            &params,
            info.virtual_daa_score,
            &source,
            &destination,
            amount_pradha,
            fee_pradha,
            feerate,
            &secret,
            inputs,
        )?;
        crate::transfer::PreparedTransfer::new(signed, &params)
    }

    pub async fn submit_prepared_transfer(&self, prepared: crate::transfer::PreparedTransfer) -> Result<String> {
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        anyhow::ensure!(info.network_id == prepared.network_id, "node network changed after preparing transfer");
        let expected = prepared.transaction_id().to_owned();
        let submitted = self
            .client
            .submit_transaction_call(None, celenium_rpc_core::SubmitTransactionRequest::new(prepared.transaction, false))
            .await
            .context("submit_transaction rejected")?
            .transaction_id
            .to_string();
        anyhow::ensure!(submitted == expected, "node returned a different transaction id");
        Ok(submitted)
    }

    pub async fn estimate_standard_fee(&self) -> Result<u64> {
        let info = self.client.get_server_info_call(None, GetServerInfoRequest {}).await.context("get_server_info failed")?;
        let params = crate::transfer::network_params(info.network_id)?;
        crate::transfer::fee_for_mass(crate::transfer::standard_transfer_mass(&params), self.transfer_feerate().await?)
    }

    pub async fn get_current_block_color(&self, block_hash: &str) -> Result<bool> {
        let hash = RpcHash::from_str(block_hash).context("Failed to parse block hash")?;
        let resp = self
            .client
            .get_current_block_color_call(None, GetCurrentBlockColorRequest { hash })
            .await
            .context("Failed to query current block color")?;
        Ok(resp.blue)
    }

    /// Start listening for block template notifications
    /// Uses RegisterForNewBlockTemplateNotifications with ticker fallback
    /// This provides immediate notifications when new blocks are available, with polling as fallback
    pub async fn start_block_template_listener<F>(self: Arc<Self>, block_wait_time: Duration, mut block_cb: F) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        let mut rx = self.notification_rx.lock().take().ok_or_else(|| anyhow::anyhow!("Notification receiver already taken"))?;

        let api_clone = Arc::clone(&self);
        tokio::spawn(async move {
            let mut restart_channel = true;
            let mut ticker = tokio::time::interval(block_wait_time);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                // Check sync state and reconnect if needed
                if let Err(e) = api_clone.wait_for_sync().await {
                    error!("error checking celeniumd sync state, attempting reconnect: {}", e);
                    // Note: gRPC client handles reconnection automatically, but we log it
                    // In Go, reconnect() is called explicitly, but Rust gRPC handles it
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    restart_channel = true;
                }

                // Re-register for notifications if needed
                if restart_channel {
                    // In Go, RegisterForNewBlockTemplateNotifications is called here when restartChannel is true
                    // In Rust, we already subscribed in new(), and the notification channel persists
                    // If the connection is lost, the gRPC client handles reconnection automatically
                    // The notification subscription should be maintained by the gRPC client
                    // If notifications stop working, we'll fall back to ticker polling
                    restart_channel = false;
                }

                // Wait for either notification or ticker timeout
                tokio::select! {
                    // Notification received
                    notification_result = rx.recv() => {
                        match notification_result {
                            Some(Notification::NewBlockTemplate(_)) => {
                                // Drain any additional notifications
                                while rx.try_recv().is_ok() {}

                                // Call callback
                                block_cb();

                                // Reset ticker
                                ticker = tokio::time::interval(block_wait_time);
                                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            }
                            Some(_) => {
                                // Other notification types - ignore
                            }
                            None => {
                                // Channel closed - exit loop
                                warn!("Block template notification channel closed");
                                break;
                            }
                        }
                    }
                    // Ticker timeout - manually check for new blocks
                    _ = ticker.tick() => {
                        block_cb();
                    }
                }
            }
        });

        Ok(())
    }

    pub async fn start_block_template_listener_with_shutdown<F>(
        self: Arc<Self>,
        block_wait_time: Duration,
        mut shutdown_rx: watch::Receiver<bool>,
        mut block_cb: F,
    ) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        let mut rx = self.notification_rx.lock().take().ok_or_else(|| anyhow::anyhow!("Notification receiver already taken"))?;

        let api_clone = Arc::clone(&self);
        tokio::spawn(async move {
            let mut restart_channel = true;
            let mut ticker = tokio::time::interval(block_wait_time);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                if let Err(e) = api_clone.wait_for_sync().await {
                    error!("error checking celeniumd sync state, attempting reconnect: {}", e);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    restart_channel = true;
                }

                if restart_channel {
                    restart_channel = false;
                }

                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    notification_result = rx.recv() => {
                        match notification_result {
                            Some(Notification::NewBlockTemplate(_)) => {
                                while rx.try_recv().is_ok() {}
                                block_cb();
                                ticker = tokio::time::interval(block_wait_time);
                                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                            }
                            Some(_) => {}
                            None => {
                                warn!("Block template notification channel closed");
                                break;
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        block_cb();
                    }
                }
            }
        });

        Ok(())
    }
}

#[async_trait::async_trait]
impl CeleniumApiTrait for CeleniumApi {
    async fn get_block_template(
        &self,
        wallet_addr: &str,
        _remote_app: &str,
        _canxium_addr: &str,
    ) -> Result<Block, Box<dyn std::error::Error + Send + Sync>> {
        CeleniumApi::get_block_template(self, wallet_addr, "", "").await.map_err(|e| {
            let error_msg = e.to_string();
            Box::new(std::io::Error::other(error_msg)) as Box<dyn std::error::Error + Send + Sync>
        })
    }

    async fn submit_block(
        &self,
        block: Block,
    ) -> Result<celenium_rpc_core::SubmitBlockResponse, Box<dyn std::error::Error + Send + Sync>> {
        CeleniumApi::submit_block(self, block)
            .await
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn get_balances_by_addresses(
        &self,
        addresses: &[String],
    ) -> Result<Vec<(String, u64)>, Box<dyn std::error::Error + Send + Sync>> {
        CeleniumApi::get_balances_by_addresses(self, addresses)
            .await
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error + Send + Sync>)
    }

    async fn get_current_block_color(&self, block_hash: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        CeleniumApi::get_current_block_color(self, block_hash)
            .await
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as Box<dyn std::error::Error + Send + Sync>)
    }
}

#[cfg(test)]
mod funding_rpc_tests {
    use super::*;
    use celenium_addresses::{Prefix, Version};
    use celenium_consensus_core::{config::params::SIMNET_PARAMS, tx::UtxoEntry};
    use celenium_txscript::pay_to_address_script;

    #[test]
    fn coinbase_funding_becomes_mature_at_the_parameter_boundary() {
        let source = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
        let script = pay_to_address_script(&source);
        let mut params = SIMNET_PARAMS;
        params.blockrate.coinbase_maturity = 10;
        let entry = UtxoEntry::new(42, script.clone(), 100, true, None);
        assert_eq!(mature_funding_total(&params, 109, &script, [entry.clone()]).unwrap(), 0);
        assert_eq!(mature_funding_total(&params, 110, &script, [entry]).unwrap(), 42);
    }

    #[test]
    fn funding_counts_only_unencumbered_source_script_outputs() {
        let source = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
        let other = Address::new(Prefix::Simnet, Version::PubKey, &[2; 32]);
        let script = pay_to_address_script(&source);
        let entries = [
            UtxoEntry::new(42, script.clone(), u64::MAX, false, None),
            UtxoEntry::new(100, pay_to_address_script(&other), 0, false, None),
            UtxoEntry::new(200, script.clone(), 0, false, Some(Default::default())),
        ];
        assert_eq!(mature_funding_total(&SIMNET_PARAMS, 0, &script, entries).unwrap(), 42);
    }

    #[test]
    fn funding_excludes_coinbase_maturity_overflow() {
        let source = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
        let script = pay_to_address_script(&source);
        let mut params = SIMNET_PARAMS;
        params.blockrate.coinbase_maturity = 10;
        let entry = UtxoEntry::new(42, script.clone(), u64::MAX, true, None);
        assert_eq!(mature_funding_total(&params, u64::MAX, &script, [entry]).unwrap(), 0);
    }

    #[test]
    fn funding_rejects_amount_sum_overflow() {
        let source = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
        let script = pay_to_address_script(&source);
        let entries = [UtxoEntry::new(u64::MAX, script.clone(), 0, false, None), UtxoEntry::new(1, script.clone(), 0, false, None)];
        assert!(mature_funding_total(&SIMNET_PARAMS, 0, &script, entries).unwrap_err().to_string().contains("overflows"));
    }

    #[test]
    fn funding_month_tracks_virtual_daa_at_consensus_month_boundaries() {
        use celenium_consensus_core::config::params::MAINNET_PARAMS;
        assert_eq!(funding_month(&MAINNET_PARAMS, 0).unwrap(), 0);
        assert_eq!(funding_month(&MAINNET_PARAMS, 26_297_999).unwrap(), 0);
        assert_eq!(funding_month(&MAINNET_PARAMS, 26_298_000).unwrap(), 1);
        assert_eq!(funding_month(&SIMNET_PARAMS, 52_596_000).unwrap(), 2);
        assert_eq!(funding_month(&MAINNET_PARAMS, u64::MAX).unwrap(), u64::MAX / 10 / crate::emission_policy::SECONDS_PER_MONTH);
    }

    #[test]
    fn funding_month_rejects_unknown_emission_timing() {
        let mut params = SIMNET_PARAMS;
        params.deflationary_phase_daa_score = 1;
        assert!(funding_month(&params, 0).is_err());
        params.deflationary_phase_daa_score = 0;
        params.blockrate.target_time_per_block = 1000;
        assert!(funding_month(&params, 0).is_err());
    }

    #[test]
    fn mempool_spends_reserve_each_exact_input_outpoint() {
        use celenium_consensus_core::{
            constants::TX_VERSION,
            subnets::SUBNETWORK_ID_NATIVE,
            tx::{Transaction, TransactionInput, TransactionOutpoint},
        };
        let outpoint = TransactionOutpoint::new(Default::default(), 1);
        let other = TransactionOutpoint::new(Default::default(), 2);
        let transaction = Transaction::new(
            TX_VERSION,
            vec![TransactionInput::new(outpoint, vec![], 0, 1)],
            vec![],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        let spent = mempool_spent_outpoints([
            celenium_rpc_core::RpcMempoolEntry::new(0, (&transaction).into(), false),
            celenium_rpc_core::RpcMempoolEntry::new(0, (&transaction).into(), true),
        ]);
        assert_eq!(spent.len(), 1);
        assert!(spent.contains(&outpoint.into()));
        assert!(!spent.contains(&other.into()));
    }

    #[test]
    fn mempool_spent_outputs_are_removed_before_funding_is_summed() {
        use celenium_rpc_core::{RpcTransactionOutpoint, RpcUtxosByAddressesEntry};
        let source = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]);
        let script = pay_to_address_script(&source);
        let spent = RpcTransactionOutpoint { transaction_id: Default::default(), index: 1 };
        let available = RpcTransactionOutpoint { transaction_id: Default::default(), index: 2 };
        let entries = vec![
            RpcUtxosByAddressesEntry {
                address: None,
                outpoint: spent,
                utxo_entry: UtxoEntry::new(100, script.clone(), 0, false, None).into(),
            },
            RpcUtxosByAddressesEntry {
                address: None,
                outpoint: available,
                utxo_entry: UtxoEntry::new(42, script.clone(), 0, false, None).into(),
            },
        ];
        let reserved = HashSet::from([spent]);
        let entries = unspent_utxos(entries, &reserved).map(|entry| entry.utxo_entry.into());
        assert_eq!(mature_funding_total(&SIMNET_PARAMS, 0, &script, entries).unwrap(), 42);
    }

    fn pending_transaction() -> crate::treasury::PendingTransfer {
        use celenium_consensus_core::tx::TransactionOutpoint;
        let secret = [1; 32];
        let key = secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, &secret).unwrap();
        let source = Address::new(Prefix::Simnet, Version::PubKey, &key.x_only_public_key().0.serialize());
        let destination = Address::new(Prefix::Simnet, Version::PubKey, &[2; 32]);
        let entry = UtxoEntry::new(200_000_000, pay_to_address_script(&source), 0, false, None);
        let signed = crate::transfer::build_transfer(
            &SIMNET_PARAMS,
            0,
            &source,
            &destination,
            100_000_000,
            None,
            0.25,
            &secret,
            vec![(TransactionOutpoint::new(RpcHash::from_u64_word(1), 0), entry.clone())],
        )
        .unwrap();
        let rpc: celenium_rpc_core::RpcTransaction = (&signed.tx).into();
        crate::treasury::PendingTransfer {
            transaction_json: serde_json::to_string(&rpc).unwrap(),
            transaction_id: signed.tx.id().to_string(),
            amount_pradha: 100_000_000,
            fee_pradha: 509,
            source: source.to_string(),
            destination: destination.to_string(),
            network: "simnet".into(),
            withdrawal: false,
            anchor: None,
            input_entries: vec![entry.into()],
        }
    }

    #[test]
    fn journaled_transfer_rejects_mismatched_payment_metadata() {
        for field in ["source", "destination", "amount", "fee", "network"] {
            let mut pending = pending_transaction();
            match field {
                "source" => pending.source = pending.destination.clone(),
                "destination" => pending.destination = pending.source.clone(),
                "amount" => pending.amount_pradha += 1,
                "fee" => pending.fee_pradha += 1,
                "network" => pending.network = "mainnet".into(),
                _ => unreachable!(),
            }
            assert!(journaled_transaction(&pending).is_err(), "accepted altered {field}");
        }
    }

    #[test]
    fn journaled_transfer_rejects_changed_signature_with_the_same_transaction_id() {
        let mut pending = pending_transaction();
        let mut tx: celenium_rpc_core::RpcTransaction = serde_json::from_str(&pending.transaction_json).unwrap();
        tx.inputs[0].signature_script[1] ^= 1;
        let core = celenium_consensus_core::tx::Transaction::try_from(tx.clone()).unwrap();
        assert_eq!(core.id().to_string(), pending.transaction_id);
        pending.transaction_json = serde_json::to_string(&tx).unwrap();
        assert!(journaled_transaction(&pending).is_err());
    }

    #[test]
    fn journaled_transfer_rejects_missing_or_forged_input_entries() {
        let mut pending = pending_transaction();
        pending.input_entries.clear();
        assert!(journaled_transaction(&pending).is_err());
        let mut pending = pending_transaction();
        pending.input_entries[0].amount += 1;
        pending.fee_pradha += 1;
        assert!(journaled_transaction(&pending).is_err());
    }

    #[test]
    fn reconciliation_rejects_a_node_from_another_network() {
        let pending = pending_transaction();
        let node = celenium_consensus_core::network::NetworkId::from_str("mainnet").unwrap();
        assert!(validate_journal_network(&pending, node).is_err());
        let node = celenium_consensus_core::network::NetworkId::from_str("simnet").unwrap();
        assert!(validate_journal_network(&pending, node).is_ok());
    }

    #[test]
    fn funding_rejects_nodes_without_consensus_utxo_attestation() {
        assert!(require_consensus_utxos("1.0.0").is_err());
        assert!(require_consensus_utxos("1.0.0+utxo-consensus-v1").is_ok());
    }

    #[test]
    fn reconciliation_progress_can_resume_on_a_later_page_with_removals() {
        let mut pending = pending_transaction();
        pending.transaction_id = RpcHash::from_u64_word(77).to_string();
        pending.anchor = Some(RpcHash::from_u64_word(10).to_string());
        let page = history_page(&[11], &[(11, &[])]);
        assert!(!apply_reconciliation_page(&mut pending, &page).unwrap());
        assert_eq!(pending.anchor, Some(RpcHash::from_u64_word(11).to_string()));
        let mut page = history_page(&[12], &[(12, &[77])]);
        page.removed_chain_block_hashes.push(RpcHash::from_u64_word(11));
        assert!(apply_reconciliation_page(&mut pending, &page).unwrap());
    }

    fn history_page(added: &[u64], accepted: &[(u64, &[u64])]) -> celenium_rpc_core::GetVirtualChainFromBlockResponse {
        celenium_rpc_core::GetVirtualChainFromBlockResponse::new(
            vec![],
            added.iter().copied().map(RpcHash::from_u64_word).collect(),
            accepted
                .iter()
                .map(|(block, ids)| celenium_rpc_core::RpcAcceptedTransactionIds {
                    accepting_block_hash: RpcHash::from_u64_word(*block),
                    accepted_transaction_ids: ids.iter().copied().map(RpcHash::from_u64_word).collect(),
                })
                .collect(),
        )
    }

    #[test]
    fn reconciliation_page_advances_anchor_after_checking_all_acceptance_data() {
        let mut pending = pending_transaction();
        pending.anchor = Some(RpcHash::from_u64_word(10).to_string());
        let page = history_page(&[11, 12], &[(11, &[99]), (12, &[98])]);

        assert!(!apply_reconciliation_page(&mut pending, &page).unwrap());
        assert_eq!(pending.anchor, Some(RpcHash::from_u64_word(12).to_string()));
    }

    #[test]
    fn reconciliation_page_finds_a_transaction_even_when_acceptance_data_is_duplicated() {
        let mut pending = pending_transaction();
        pending.transaction_id = RpcHash::from_u64_word(77).to_string();
        pending.anchor = Some(RpcHash::from_u64_word(10).to_string());
        let page = history_page(&[11], &[(11, &[77, 77])]);

        assert!(apply_reconciliation_page(&mut pending, &page).unwrap());
        assert_eq!(pending.anchor, Some(RpcHash::from_u64_word(10).to_string()));
    }

    #[test]
    fn reconciliation_page_does_not_advance_on_removals_or_duplicate_anchor() {
        let mut pending = pending_transaction();
        pending.anchor = Some(RpcHash::from_u64_word(10).to_string());
        let page = history_page(&[10], &[]);

        assert!(!apply_reconciliation_page(&mut pending, &page).unwrap());
        assert_eq!(pending.anchor, Some(RpcHash::from_u64_word(10).to_string()));

        let page = history_page(&[], &[]);
        assert!(!apply_reconciliation_page(&mut pending, &page).unwrap());
        assert_eq!(pending.anchor, Some(RpcHash::from_u64_word(10).to_string()));
    }

    #[test]
    fn journaled_transfer_rejects_a_transaction_id_mismatch() {
        let mut pending = pending_transaction();
        assert!(journaled_transaction(&pending).is_ok());
        pending.transaction_id = Default::default();
        assert!(journaled_transaction(&pending).is_err());
    }

    #[test]
    fn journaled_transfer_rejects_modified_transaction_contents() {
        let mut pending = pending_transaction();
        let mut transaction: celenium_rpc_core::RpcTransaction = serde_json::from_str(&pending.transaction_json).unwrap();
        transaction.lock_time = 1;
        pending.transaction_json = serde_json::to_string(&transaction).unwrap();
        assert!(journaled_transaction(&pending).is_err());
    }
}
