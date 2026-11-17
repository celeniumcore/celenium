use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::{celeniumapi::CeleniumApi, faucet::FaucetState, faucet_signer::FaucetSigner};

pub const TREASURY_JOURNAL_PATH: &str = "reservoir_transfers.json";

#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreasuryStatus {
    pub surplus_address: Option<String>,
    pub bootstrap_source: Option<String>,
    pub pending_txid: Option<String>,
    pub last_txid: Option<String>,
    pub total_returned_pradha: u64,
    pub error: Option<String>,
}

static STATUS: std::sync::OnceLock<Mutex<TreasuryStatus>> = std::sync::OnceLock::new();

pub fn status() -> TreasuryStatus {
    STATUS.get_or_init(|| Mutex::new(TreasuryStatus::default())).lock().unwrap().clone()
}

pub fn set_routes(source: Option<String>, destination: Option<String>) {
    let mut status = STATUS.get_or_init(|| Mutex::new(TreasuryStatus::default())).lock().unwrap();
    status.bootstrap_source = source;
    status.surplus_address = destination;
}

fn route_coinbase<'a>(
    worker: &'a str,
    pool: Option<&'a str>,
    bootstrap: Option<&str>,
    reservoir: Option<&'a str>,
    funds: Option<crate::reservoir_funds::FundingSnapshot>,
    now: u64,
) -> &'a str {
    if bootstrap == Some(worker)
        && funds.is_some_and(|f| f.is_fresh(now) && f.mature_pradha < crate::emission_policy::floor_pradha(f.month))
    {
        if let Some(reservoir) = reservoir {
            return reservoir;
        }
    }
    pool.unwrap_or(worker)
}

pub fn coinbase_address(worker: &str, pool: Option<&str>) -> String {
    let status = status();
    let address = crate::prom::faucet_address();
    let funding = crate::prom::faucet_state_snapshot().and_then(|s| {
        let state = s.lock().unwrap();
        bootstrap_funding(state.funding_sources, FaucetState::now_secs(), state.total_outstanding())
    });
    route_coinbase(worker, pool, status.bootstrap_source.as_deref(), address.as_deref(), funding, FaucetState::now_secs()).into()
}

fn bootstrap_funding(
    sources: Option<crate::reservoir_funds::FaucetFundingSnapshot>,
    now: u64,
    outstanding: u64,
) -> Option<crate::reservoir_funds::FundingSnapshot> {
    sources.and_then(|funds| funds.combined(now, crate::emission_policy::floor_pradha(funds.operational.month), outstanding))
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PendingTransfer {
    pub transaction_json: String,
    pub transaction_id: String,
    pub amount_pradha: u64,
    pub fee_pradha: u64,
    pub source: String,
    pub destination: String,
    pub network: String,
    #[serde(default)]
    pub withdrawal: bool,
    #[serde(default)]
    pub anchor: Option<String>,
    #[serde(default)]
    pub input_entries: Vec<celenium_rpc_core::RpcUtxoEntry>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct TreasuryJournal {
    pub pending: Option<PendingTransfer>,
    pub last_txid: Option<String>,
    pub total_returned_pradha: u64,
    pub last_sweep_secs: u64,
}

impl TreasuryJournal {
    pub fn can_prepare(&self) -> bool {
        self.pending.is_none()
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".treasury-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(self)?)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)?;
            #[cfg(unix)]
            std::fs::File::open(dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temp);
        }
        result
    }
}

pub fn start(
    api: Arc<CeleniumApi>,
    state: Arc<Mutex<FaucetState>>,
    signer: Option<Arc<FaucetSigner>>,
    source: Option<String>,
    destination: Option<String>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let mut journal = TreasuryJournal::load(Path::new(TREASURY_JOURNAL_PATH))?;
    if let (Some(source), Some(destination)) = (&source, &destination) {
        let a = celenium_addresses::Address::try_from(source.as_str())?;
        let b = celenium_addresses::Address::try_from(destination.as_str())?;
        anyhow::ensure!(a != b && a.prefix == b.prefix, "surplus destination must differ and use the reservoir network");
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = shutdown.changed() => if *shutdown.borrow() { break },
                _ = interval.tick() => {
                    let _guard = crate::prom::FAUCET_WITHDRAW_LOCK.lock().await;
                    let result = tokio::time::timeout(std::time::Duration::from_secs(15),
                        cycle(&api, &state, signer.as_deref(), source.as_deref(), destination.as_deref(), &mut journal)
                    ).await;
                    let error = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("{e:#}")),
                        Err(_) => Some("reservoir node request timed out".into()),
                    };
                    if error.is_some() || journal.pending.is_some() { state.lock().unwrap().invalidate_funding(); }
                    let mut status = STATUS.get_or_init(|| Mutex::new(TreasuryStatus::default())).lock().unwrap();
                    status.error = error;
                    status.pending_txid = journal.pending.as_ref().map(|p| p.transaction_id.clone());
                    status.last_txid = journal.last_txid.clone();
                    status.total_returned_pradha = journal.total_returned_pradha;
                }
            }
        }
    });
    Ok(())
}

async fn cycle(
    api: &CeleniumApi,
    state: &Mutex<FaucetState>,
    signer: Option<&FaucetSigner>,
    source: Option<&str>,
    destination: Option<&str>,
    journal: &mut TreasuryJournal,
) -> anyhow::Result<()> {
    *journal = TreasuryJournal::load(Path::new(TREASURY_JOURNAL_PATH))?;
    if let Some(mut pending) = journal.pending.clone() {
        state.lock().unwrap().invalidate_funding();
        if api.journaled_transfer_accepted(&mut pending).await? {
            let mut candidate = journal.clone();
            if pending.withdrawal {
                let mut state = state.lock().unwrap();
                anyhow::ensure!(
                    state.settle_withdrawal(&pending.transaction_id, &pending.destination, pending.amount_pradha, pending.fee_pradha),
                    "withdrawal ledger debit failed"
                );
                state.save(Path::new(crate::faucet::FAUCET_LEDGER_PATH))?;
            } else {
                candidate.total_returned_pradha = candidate.total_returned_pradha.saturating_add(pending.amount_pradha);
            }
            candidate.last_txid = Some(pending.transaction_id.clone());
            if !pending.withdrawal {
                candidate.last_sweep_secs = FaucetState::now_secs();
            }
            candidate.pending = None;
            candidate.save(Path::new(TREASURY_JOURNAL_PATH))?;
            *journal = candidate;
        } else {
            // Persist before every retry in case a previous fsync failed before broadcasting.
            journal.pending = Some(pending.clone());
            journal.save(Path::new(TREASURY_JOURNAL_PATH))?;
            api.submit_journaled_transfer(&pending).await?;
            return Ok(());
        }
    }
    let Some(source) = source else { return Ok(()) };
    let backup = crate::prom::faucet_reserve();
    let sources = crate::faucet_funding::refresh(api, state, source, backup.as_ref().map(|(address, _)| address.as_str())).await?;
    let funds = sources.operational;
    let now = FaucetState::now_secs();
    let (outstanding, reserve, backup_eligible) = {
        let state = state.lock().unwrap();
        anyhow::ensure!(!state.storage_uncertain, "faucet ledger requires recovery");
        let combined = state.funding.filter(|funds| funds.is_fresh(now)).ok_or_else(|| anyhow::anyhow!("faucet funding expired"))?;
        (state.total_outstanding(), state.service_reserve_pradha(), combined.mature_pradha.saturating_sub(funds.mature_pradha))
    };
    let (Some(signer), Some(destination)) = (signer, destination) else { return Ok(()) };
    if now.saturating_sub(journal.last_sweep_secs) < 60 {
        return Ok(());
    }
    signer.ensure_matches(source)?;
    let floor = crate::emission_policy::floor_pradha(funds.month);
    let provisional = crate::reservoir_funds::sweep_amount_with_funding_sources(
        funds.mature_pradha,
        backup_eligible,
        floor,
        outstanding,
        reserve,
        1_000_000,
    );
    if provisional < 100_000_000 {
        return Ok(());
    }
    let prepared = api.prepare_surplus_transfer(source, destination, provisional, signer.secret_hex()).await?;
    let amount = prepared.transaction.outputs.first().ok_or_else(|| anyhow::anyhow!("surplus quote has no output"))?.value;
    if amount < 100_000_000 {
        return Ok(());
    }
    let permitted = crate::reservoir_funds::sweep_amount_with_funding_sources(
        funds.mature_pradha,
        backup_eligible,
        floor,
        outstanding,
        reserve,
        prepared.fee_pradha(),
    );
    anyhow::ensure!(amount <= permitted, "surplus fee exceeds reserved headroom");
    journal.pending = Some(PendingTransfer {
        transaction_json: prepared.transaction_json()?,
        transaction_id: prepared.transaction_id().to_owned(),
        amount_pradha: amount,
        fee_pradha: prepared.fee_pradha(),
        source: source.into(),
        destination: destination.into(),
        network: prepared.network_id.to_string(),
        withdrawal: false,
        anchor: Some(api.transfer_anchor().await?),
        input_entries: prepared.input_entries(),
    });
    state.lock().unwrap().invalidate_funding();
    journal.save(Path::new(TREASURY_JOURNAL_PATH))?;
    api.submit_prepared_transfer(prepared).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_withdrawal_replay_keeps_signed_transaction_and_source() {
        let pending = PendingTransfer {
            transaction_json: "signed-transaction-fixture".into(),
            transaction_id: "same-transaction".into(),
            amount_pradha: 1000,
            fee_pradha: 275,
            source: "reserve".into(),
            destination: "claimant".into(),
            network: "simnet".into(),
            withdrawal: true,
            anchor: None,
            input_entries: vec![],
        };
        let journal = TreasuryJournal { pending: Some(pending), ..Default::default() };
        let restored: TreasuryJournal = serde_json::from_slice(&serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(!restored.can_prepare());
        let pending = restored.pending.unwrap();
        assert_eq!(pending.source, "reserve");
        assert!(pending.withdrawal);
        assert_eq!(pending.transaction_json, "signed-transaction-fixture");
        assert_eq!(pending.transaction_id, "same-transaction");
        assert_eq!(restored.total_returned_pradha, 0);
    }

    #[test]
    fn bootstrap_stops_when_operational_and_reserve_meet_the_shared_floor() {
        let operational = 200_000_000_000_000;
        let backup = 50_000_000_000_000;
        let funds = crate::reservoir_funds::FaucetFundingSnapshot {
            operational: crate::reservoir_funds::FundingSnapshot { mature_pradha: operational, month: 0, observed_at: 100 },
            reserve: Some(crate::reservoir_funds::FundingSnapshot { mature_pradha: backup, month: 0, observed_at: 100 }),
            reserve_configured: true,
        };
        let combined = bootstrap_funding(Some(funds), 100, 0).unwrap();
        assert_eq!(combined.mature_pradha, crate::emission_policy::floor_pradha(0));
        assert_eq!(route_coinbase("operator", None, Some("operator"), Some("reservoir"), Some(combined), 100), "operator");
    }

    #[test]
    fn only_operator_bootstrap_worker_funds_reservoir() {
        let funds = crate::reservoir_funds::FundingSnapshot { mature_pradha: 0, month: 0, observed_at: 100 };
        assert_eq!(route_coinbase("operator", None, Some("operator"), Some("reservoir"), Some(funds), 100), "reservoir");
        assert_eq!(route_coinbase("other", None, Some("operator"), Some("reservoir"), Some(funds), 100), "other");
        assert_eq!(route_coinbase("operator", None, Some("operator"), Some("reservoir"), Some(funds), 131), "operator");
        let full = crate::reservoir_funds::FundingSnapshot { mature_pradha: crate::emission_policy::floor_pradha(0), ..funds };
        assert_eq!(route_coinbase("operator", None, Some("operator"), Some("reservoir"), Some(full), 100), "operator");
    }

    #[test]
    fn pending_transfer_survives_restart_without_becoming_spendable() {
        let path = std::env::temp_dir().join(format!("reservoir-journal-{}.json", uuid::Uuid::new_v4()));
        let mut journal = TreasuryJournal::default();
        journal.pending = Some(PendingTransfer {
            transaction_json: "{\"test\":true}".into(),
            transaction_id: "test-transaction".into(),
            amount_pradha: 10_000,
            fee_pradha: 500,
            source: "source".into(),
            destination: "miner-fund".into(),
            network: "simnet".into(),
            withdrawal: false,
            anchor: None,
            input_entries: vec![],
        });
        journal.save(&path).unwrap();
        let restored = TreasuryJournal::load(&path).unwrap();
        assert_eq!(restored.pending.as_ref().unwrap().transaction_id, "test-transaction");
        assert!(!restored.can_prepare());
        std::fs::remove_file(path).unwrap();
    }
}
