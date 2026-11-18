//! Reward partisipasi dibagi dari pool harian yang didukung dana matang.
//! Klaim mencatat saldo off-chain; penarikan memerlukan dana reservoir on-chain.

use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const FAUCET_MAX_REWARD_PRADHA: u64 = crate::reward_policy::INITIAL_MAX_REWARD_PRADHA;

/// Plafon kewajiban klaim faucet: 500.000 CEN.
pub const FAUCET_MAX_OUTSTANDING_PRADHA: u64 = crate::reservoir_funds::FAUCET_RESERVE_LIMIT_PRADHA;

/// File tempat ledger faucet disimpan (relatif ke working directory bridge).
pub const FAUCET_LEDGER_PATH: &str = "faucet_ledger.json";

/// Maksimal address wallet berbeda per device (Android ID) — anti-sybil lunak.
pub const FAUCET_MAX_ACCOUNTS_PER_DEVICE: usize = 5;

/// Jeda minimum antar-withdraw untuk satu alamat, dalam detik.
/// Withdraw tanpa minimum nominal berarti tiap penarikan tetap memakan satu fee
/// on-chain dari reservoir. Tanpa jeda, saldo kecil bisa ditarik berulang kali
/// dan biayanya jauh melampaui nilai yang dikirim.
pub const FAUCET_WITHDRAW_COOLDOWN_SECS: u64 = 3_600;

/// Anggaran fee on-chain per hari, dalam pradha (2,5 CEN).
/// Ini backstop terakhir: berapa pun jumlah alamat yang dipakai, reservoir tidak
/// pernah membayar lebih dari plafon ini untuk biaya transaksi per hari.
pub const FAUCET_DAILY_FEE_BUDGET_PRADHA: u64 = 250_000_000;

/// Accumulation avoids outputs below the standard storage-mass limit.
pub const FAUCET_MIN_WITHDRAW_PRADHA: u64 = 2_500_000;

pub const FAUCET_MIN_REWARD_PRADHA: u64 = crate::reward_policy::INITIAL_MIN_REWARD_PRADHA;

/// Detik per hari (hari Unix untuk throttling klaim).
const DAY_SECS: u64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct FaucetEntry {
    pub balance_pradha: u64,
    pub last_claim_day: u64,
    pub total_claimed_pradha: u64,
    /// Jumlah reward kumulatif yang sudah diklaim pada hari `last_claim_day`.
    #[serde(default)]
    pub last_claim_reward_pradha: u64,
    /// Kuota reward untuk hari `last_claim_day`, ditetapkan pada klaim pertama.
    #[serde(default)]
    pub daily_reward_pradha: u64,
    /// Porsi yang diberikan pada klaim terakhir.
    #[serde(default)]
    pub last_claim_payout_pradha: u64,
    /// Detik Unix penarikan on-chain terakhir (0 bila belum pernah).
    #[serde(default)]
    pub last_withdraw_secs: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FaucetState {
    #[serde(skip)]
    pub(crate) storage_uncertain: bool,
    #[serde(skip)]
    pub(crate) funding_snapshot_dirty: bool,
    #[serde(skip)]
    pub(crate) funding: Option<crate::reservoir_funds::FundingSnapshot>,
    #[serde(skip)]
    pub(crate) funding_sources: Option<crate::reservoir_funds::FaucetFundingSnapshot>,
    #[serde(skip)]
    network_prefix: Option<celenium_addresses::Prefix>,
    ledger: HashMap<String, FaucetEntry>,
    /// device_id (Android ID) → set address wallet yang pernah klaim.
    devices: HashMap<String, HashSet<String>>,
    /// Total pradha dikeluarkan pada hari Unix `claim_day`.
    claimed_today: u64,
    /// Hari Unix yang sedang dihitung untuk `claimed_today`.
    claim_day: u64,
    /// Jumlah klaim berhasil pada `claim_day`.
    #[serde(default)]
    claimants_today: u64,
    /// Jumlah klaim pada hari aktif terakhir sebelum `claim_day`.
    ///
    /// Inilah pengukur permintaan yang dipakai untuk membagi anggaran harian.
    /// Memakai angka hari sebelumnya (bukan maksimum sepanjang masa) membuat
    /// reward pulih sendiri ketika peserta berkurang.
    #[serde(default)]
    last_day_claimants: u64,
    /// Total fee on-chain yang sudah dibayar reservoir pada `fee_day`.
    #[serde(default)]
    fees_today_pradha: u64,
    /// Hari Unix yang sedang dihitung untuk `fees_today_pradha`.
    #[serde(default)]
    fee_day: u64,
    #[serde(default)]
    peak_daily_spend_pradha: u64,
    #[serde(default)]
    settled_withdrawal_txids: HashSet<String>,
    #[serde(default)]
    funding_budget_day: u64,
    #[serde(default)]
    funding_daily_cap_pradha: u64,
    #[serde(default)]
    funding_reward_pradha: Option<u64>,
    #[serde(default)]
    funding_reward_month: u64,
    #[serde(default)]
    funding_expected_claimants: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    AlreadyClaimed,
    StorageUnavailable,
    FundingUnavailable,
    DailyCapReached,
    ReservoirFull,
    InvalidAddress,
    EmptyAddress,
    TooManyAccounts,
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ClaimError::AlreadyClaimed => "daily reward already claimed; try again after 00:00 UTC",
            ClaimError::StorageUnavailable => "faucet storage needs recovery; retry after the operator restores it",
            ClaimError::FundingUnavailable => "faucet has no verified free funds; retry after the reservoir is funded",
            ClaimError::DailyCapReached => "daily faucet cap reached",
            ClaimError::ReservoirFull => "faucet liability cap reached",
            ClaimError::InvalidAddress => "address is not a valid Celenium address",
            ClaimError::EmptyAddress => "address is empty",
            ClaimError::TooManyAccounts => "device has too many accounts",
        };
        f.write_str(s)
    }
}

impl ClaimError {
    pub fn code(self) -> &'static str {
        match self {
            Self::AlreadyClaimed => "AlreadyClaimed",
            Self::StorageUnavailable => "StorageUnavailable",
            Self::FundingUnavailable => "FundingUnavailable",
            Self::DailyCapReached => "DailyCapReached",
            Self::ReservoirFull => "ReservoirFull",
            Self::InvalidAddress => "InvalidAddress",
            Self::EmptyAddress => "EmptyAddress",
            Self::TooManyAccounts => "TooManyAccounts",
        }
    }
}

impl std::error::Error for ClaimError {}

struct ClaimAmounts {
    quota: u64,
    accrued: u64,
    paid: u64,
    pending: u64,
    payout: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WithdrawError {
    StorageUnavailable,
    /// Alamat menarik lagi sebelum jeda antar-withdraw lewat.
    Cooldown {
        remaining_secs: u64,
    },
    /// Anggaran fee on-chain hari ini sudah habis.
    FeeBudgetExhausted {
        used_pradha: u64,
        budget_pradha: u64,
    },
}

impl std::fmt::Display for WithdrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WithdrawError::StorageUnavailable => f.write_str("faucet storage needs recovery; withdrawals paused"),
            WithdrawError::Cooldown { remaining_secs } => {
                write!(f, "please wait {remaining_secs} seconds before withdrawing again")
            }
            WithdrawError::FeeBudgetExhausted { used_pradha, budget_pradha } => {
                write!(f, "daily faucet fee budget exhausted ({used_pradha}/{budget_pradha} pradha); try again after 00:00 UTC")
            }
        }
    }
}

impl std::error::Error for WithdrawError {}

impl FaucetState {
    pub fn current_day() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / DAY_SECS).unwrap_or(0)
    }

    pub fn claimants_today(&self) -> u64 {
        if Self::current_day() == self.claim_day { self.claimants_today } else { 0 }
    }

    fn demand_at(&self, now: u64) -> (u64, u64) {
        let day = now / DAY_SECS;
        if day == self.claim_day {
            (self.claimants_today, self.last_day_claimants)
        } else if day.checked_sub(self.claim_day) == Some(1) {
            (0, self.claimants_today)
        } else {
            (0, 0)
        }
    }

    pub fn expected_claimants_today(&self) -> u64 {
        self.funding_expected_claimants.max(1)
    }

    pub fn reward_pradha_today(&self) -> u64 {
        let quota = self.daily_reward_at(Self::now_secs());
        let cap = self.daily_cap_now();
        if cap.saturating_sub(self.claimed_today()) >= quota { quota } else { 0 }
    }

    fn daily_reward_at(&self, now: u64) -> u64 {
        if self.funding_budget_day == now / DAY_SECS { self.funding_reward_pradha.unwrap_or(0) } else { 0 }
    }

    pub fn reward_bounds_today(&self) -> (u64, u64) {
        crate::reward_policy::reward_bounds(self.funding_reward_month)
    }

    pub fn reward_month(&self) -> u64 {
        self.funding_reward_month
    }

    pub fn validate_address(address: &str) -> Result<(), ClaimError> {
        if address.trim().is_empty() {
            return Err(ClaimError::EmptyAddress);
        }
        let parsed = celenium_addresses::Address::try_from(address).map_err(|_| ClaimError::InvalidAddress)?;
        if parsed.to_string() != address {
            return Err(ClaimError::InvalidAddress);
        }
        Ok(())
    }

    fn claim_amounts_at(&self, address: &str, now: u64) -> ClaimAmounts {
        let day = now / DAY_SECS;
        let entry = self.balance(address);
        // A policy change cannot increase an entitlement already recorded for today.
        let today = entry.filter(|e| e.last_claim_day == day);
        let quota = today
            .map(|e| if e.daily_reward_pradha == 0 { e.last_claim_reward_pradha.max(1_000) } else { e.daily_reward_pradha })
            .unwrap_or_else(|| self.daily_reward_at(now));
        let paid = today.map(|e| if e.daily_reward_pradha == 0 { quota } else { e.last_claim_reward_pradha }).unwrap_or(0);
        let pending = entry
            .filter(|e| e.last_claim_day < day && e.daily_reward_pradha > 0)
            .map(|e| e.daily_reward_pradha.saturating_sub(e.last_claim_reward_pradha))
            .unwrap_or(0);
        ClaimAmounts { quota, accrued: quota, paid, pending, payout: pending.saturating_add(quota.saturating_sub(paid)) }
    }

    fn check_payout_at(&self, address: &str, now: u64, payout: u64) -> Result<(), ClaimError> {
        if self.storage_uncertain {
            return Err(ClaimError::StorageUnavailable);
        }
        Self::validate_address(address)?;
        if self.network_prefix.is_some_and(|prefix| celenium_addresses::Address::try_from(address).is_ok_and(|a| a.prefix != prefix)) {
            return Err(ClaimError::InvalidAddress);
        }
        let funding = self.funding.filter(|f| f.is_fresh(now)).ok_or(ClaimError::FundingUnavailable)?;
        let day = now / DAY_SECS;
        // Jangan membuka ulang hari yang sudah dilewati setelah koreksi jam server.
        if day < self.claim_day || self.balance(address).is_some_and(|e| e.last_claim_day > day) {
            return Err(ClaimError::AlreadyClaimed);
        }
        let cap = self.outstanding_cap_at(now);
        let outstanding = self.total_outstanding();
        if outstanding >= cap || payout > cap.saturating_sub(outstanding) {
            return Err(ClaimError::ReservoirFull);
        }
        let free = crate::reservoir_funds::available_for_claims(funding.mature_pradha, outstanding);
        if free == 0 && payout == 0 {
            return Err(ClaimError::FundingUnavailable);
        }
        if payout > free {
            return Err(ClaimError::FundingUnavailable);
        }
        let daily_cap = self.daily_cap_at(now);
        let used = if self.claim_day == day { self.claimed_today } else { 0 };
        if used >= daily_cap || payout > daily_cap.saturating_sub(used) {
            return Err(ClaimError::DailyCapReached);
        }
        if payout == 0 {
            return Err(if self.daily_reward_at(now) == 0 && self.balance(address).is_none_or(|e| e.last_claim_day != day) {
                ClaimError::FundingUnavailable
            } else if self.balance(address).is_some_and(|e| e.last_claim_day == day) {
                ClaimError::AlreadyClaimed
            } else {
                ClaimError::DailyCapReached
            });
        }
        Ok(())
    }

    pub fn claimable_now(&self, address: &str) -> u64 {
        self.claimable_at(address, Self::now_secs())
    }

    fn claimable_at(&self, address: &str, now: u64) -> u64 {
        let amount = self.claim_amounts_at(address, now).payout;
        if self.check_payout_at(address, now, amount).is_ok() { amount } else { 0 }
    }

    pub fn balance_response(&self, address: &str, now: u64) -> serde_json::Value {
        if let Err(e) = Self::validate_address(address) {
            return serde_json::json!({ "error": e.to_string(), "errorCode": e.code() });
        }
        let amounts = self.claim_amounts_at(address, now);
        let entry = self.balance(address).copied().unwrap_or_default();
        let reward = self.claimable_at(address, now);
        serde_json::json!({
            "address": address,
            "balancePradha": entry.balance_pradha,
            "totalClaimedPradha": entry.total_claimed_pradha,
            "rewardPradha": reward,
            "dailyRewardPradha": amounts.quota,
            "accruedTodayPradha": amounts.accrued,
            "claimedTodayPradha": amounts.paid,
            "pendingPreviousPradha": amounts.pending,
            "canClaimToday": reward > 0,
            "canStartDrip": false,
            "serverTimeSecs": now,
            "claimPeriodSecs": DAY_SECS,
            "rewardModel": crate::reward_policy::REWARD_MODEL,
            "minRewardPradha": self.reward_bounds_today().0,
            "maxRewardPradha": self.reward_bounds_today().1,
            "rewardEmissionMonth": self.funding_reward_month,
            "halvingIntervalMonths": crate::reward_policy::HALVING_MONTHS,
            "minWithdrawPradha": FAUCET_MIN_WITHDRAW_PRADHA,
            "dripDurationSecs": 0,
            "dripModel": "none",
        })
    }

    pub fn claim(&mut self, address: &str, device_id: &str) -> Result<FaucetEntry, ClaimError> {
        self.claim_at(address, device_id, Self::now_secs())
    }

    pub(crate) fn claim_at(&mut self, address: &str, device_id: &str, now: u64) -> Result<FaucetEntry, ClaimError> {
        Self::validate_address(address)?;
        let did = device_id.trim();
        if !did.is_empty()
            && self.devices.get(did).is_some_and(|set| !set.contains(address) && set.len() >= FAUCET_MAX_ACCOUNTS_PER_DEVICE)
        {
            return Err(ClaimError::TooManyAccounts);
        }
        let amounts = self.claim_amounts_at(address, now);
        self.check_payout_at(address, now, amounts.payout)?;
        let day = now / DAY_SECS;
        let first_today = self.balance(address).is_none_or(|e| e.last_claim_day != day);
        self.roll_day_at(now);
        let entry = self.ledger.entry(address.to_string()).or_default();
        entry.balance_pradha = entry.balance_pradha.saturating_add(amounts.payout);
        entry.total_claimed_pradha = entry.total_claimed_pradha.saturating_add(amounts.payout);
        entry.last_claim_day = day;
        entry.last_claim_reward_pradha = amounts.paid.max(amounts.accrued);
        entry.daily_reward_pradha = amounts.quota;
        entry.last_claim_payout_pradha = amounts.payout;
        let updated = *entry;
        self.claimed_today = self.claimed_today.saturating_add(amounts.payout);
        self.peak_daily_spend_pradha = self.peak_daily_spend_pradha.max(self.claimed_today.saturating_add(self.fees_today()));
        if first_today {
            self.claimants_today = self.claimants_today.saturating_add(1);
        }
        if !did.is_empty() {
            self.devices.entry(did.to_string()).or_default().insert(address.to_string());
        }
        Ok(updated)
    }

    fn roll_day_at(&mut self, now: u64) {
        let day = now / DAY_SECS;
        if day > self.claim_day {
            self.last_day_claimants = self.claimants_today;
            self.claim_day = day;
            self.claimed_today = 0;
            self.claimants_today = 0;
        }
    }

    /// Saldo tercatat untuk `address`, bila ada.
    pub fn balance(&self, address: &str) -> Option<&FaucetEntry> {
        self.ledger.get(address)
    }

    /// Reward kumulatif yang sudah diklaim oleh `address` pada hari ini.
    pub fn claimed_today_for(&self, address: &str) -> u64 {
        let day = Self::current_day();
        self.ledger.get(address).filter(|entry| entry.last_claim_day == day).map(|entry| entry.last_claim_reward_pradha).unwrap_or(0)
    }

    /// Nominal pada klaim terakhir oleh `address` hari ini.
    pub fn claim_reward_for(&self, address: &str) -> u64 {
        let day = Self::current_day();
        self.ledger.get(address).filter(|entry| entry.last_claim_day == day).map(|entry| entry.last_claim_payout_pradha).unwrap_or(0)
    }

    pub fn now_secs() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
    }

    fn roll_fee_day(&mut self) {
        let day = Self::current_day();
        if day != self.fee_day {
            self.fee_day = day;
            self.fees_today_pradha = 0;
        }
    }

    /// Periksa apakah `address` boleh menarik sekarang.
    ///
    /// Dua pengaman, keduanya menahan biaya on-chain yang dibayar reservoir:
    /// plafon fee harian global (dicek dulu: ini batas keras) dan jeda per
    /// alamat (satu penarikan per jam).
    pub fn check_withdraw(&self, address: &str) -> Result<(), WithdrawError> {
        if self.storage_uncertain {
            return Err(WithdrawError::StorageUnavailable);
        }
        let used = if self.fee_day == Self::current_day() { self.fees_today_pradha } else { 0 };
        if used >= FAUCET_DAILY_FEE_BUDGET_PRADHA {
            return Err(WithdrawError::FeeBudgetExhausted { used_pradha: used, budget_pradha: FAUCET_DAILY_FEE_BUDGET_PRADHA });
        }
        let now = Self::now_secs();
        if let Some(e) = self.ledger.get(address)
            && e.last_withdraw_secs > 0
        {
            let elapsed = now.saturating_sub(e.last_withdraw_secs);
            if elapsed < FAUCET_WITHDRAW_COOLDOWN_SECS {
                return Err(WithdrawError::Cooldown { remaining_secs: FAUCET_WITHDRAW_COOLDOWN_SECS.saturating_sub(elapsed) });
            }
        }
        Ok(())
    }

    pub fn check_withdraw_fee(&self, address: &str, fee_pradha: u64) -> Result<(), WithdrawError> {
        self.check_withdraw(address)?;
        let used = if self.fee_day == Self::current_day() { self.fees_today_pradha } else { 0 };
        if fee_pradha > FAUCET_DAILY_FEE_BUDGET_PRADHA.saturating_sub(used) {
            return Err(WithdrawError::FeeBudgetExhausted { used_pradha: used, budget_pradha: FAUCET_DAILY_FEE_BUDGET_PRADHA });
        }
        Ok(())
    }

    /// Catat penarikan yang berhasil: debit saldo, set waktu, dan tambah pemakaian fee.
    pub fn record_withdraw(&mut self, address: &str, amount_pradha: u64, fee_pradha: u64) -> bool {
        self.roll_fee_day();
        let now = Self::now_secs();
        let Some(e) = self.ledger.get_mut(address) else {
            return false;
        };
        if e.balance_pradha < amount_pradha {
            return false;
        }
        e.balance_pradha -= amount_pradha;
        e.last_withdraw_secs = now;
        self.fees_today_pradha = self.fees_today_pradha.saturating_add(fee_pradha);
        self.peak_daily_spend_pradha = self.peak_daily_spend_pradha.max(self.claimed_today().saturating_add(self.fees_today_pradha));
        self.invalidate_funding();
        true
    }

    /// Kurangi saldo (dipanggil setelah withdraw on-chain berhasil).
    pub fn settle_withdrawal(&mut self, txid: &str, address: &str, amount: u64, fee: u64) -> bool {
        if self.settled_withdrawal_txids.contains(txid) {
            return true;
        }
        if !self.record_withdraw(address, amount, fee) {
            return false;
        }
        self.settled_withdrawal_txids.insert(txid.into());
        true
    }

    pub fn debit(&mut self, address: &str, amount_pradha: u64) -> bool {
        if let Some(e) = self.ledger.get_mut(address)
            && e.balance_pradha >= amount_pradha
        {
            e.balance_pradha -= amount_pradha;
            return true;
        }
        false
    }

    /// Total saldo tergabung (untuk laporan/reservoir check).
    pub fn total_outstanding(&self) -> u64 {
        self.ledger.values().fold(0u64, |total, e| total.saturating_add(e.balance_pradha))
    }

    pub fn len(&self) -> usize {
        self.ledger.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ledger.is_empty()
    }

    /// Klaim hari ini yang sudah dihitung (0 bila sudah ganti hari).
    pub fn claimed_today(&self) -> u64 {
        if Self::current_day() != self.claim_day { 0 } else { self.claimed_today }
    }

    /// Cap harian dinamis saat ini (pradha), mengikuti kebijakan emisi.
    pub fn daily_cap_now(&self) -> u64 {
        self.daily_cap_at(Self::now_secs())
    }

    fn daily_cap_at(&self, now: u64) -> u64 {
        let Some(funding) = self.funding.filter(|f| f.is_fresh(now)) else { return 0 };
        let used = if now / DAY_SECS == self.claim_day { self.claimed_today } else { 0 };
        if self.funding_budget_day != now / DAY_SECS {
            return 0;
        }
        let outstanding = self.total_outstanding();
        let liability_headroom = self.outstanding_cap_at(now).saturating_sub(outstanding);
        let free = crate::reservoir_funds::available_for_claims(funding.mature_pradha, outstanding);
        self.funding_daily_cap_pradha.min(used.saturating_add(free.min(liability_headroom)))
    }

    /// Existing claims remain visible even when their backing falls below the liability cap.
    pub fn outstanding_cap_now(&self) -> u64 {
        self.outstanding_cap_at(Self::now_secs())
    }

    fn outstanding_cap_at(&self, _now: u64) -> u64 {
        FAUCET_MAX_OUTSTANDING_PRADHA.max(self.total_outstanding())
    }

    pub fn update_funding(&mut self, mature_pradha: u64, month: u64, now: u64) {
        self.update_funding_sources(
            crate::reservoir_funds::FaucetFundingSnapshot {
                operational: crate::reservoir_funds::FundingSnapshot { mature_pradha, month, observed_at: now },
                reserve: None,
                reserve_configured: false,
            },
            now,
        );
    }

    pub fn update_funding_sources(&mut self, sources: crate::reservoir_funds::FaucetFundingSnapshot, now: u64) -> bool {
        let floor = crate::emission_policy::floor_pradha(sources.operational.month);
        let Some(funding) = sources.combined_with_service_reserve(now, floor, self.total_outstanding(), self.service_reserve_pradha())
        else {
            self.invalidate_funding();
            return false;
        };
        self.funding_sources = Some(sources);
        self.funding = Some(funding);
        let new_day = self.funding_budget_day != now / DAY_SECS;
        let initialize = new_day || self.funding_daily_cap_pradha == 0 || self.funding_reward_pradha.is_none();
        if new_day || self.funding_daily_cap_pradha == 0 {
            self.funding_daily_cap_pradha = crate::reservoir_funds::cash_daily_cap(
                funding.mature_pradha,
                self.total_outstanding(),
                crate::emission_policy::daily_cap_pradha(funding.month),
            );
            self.funding_budget_day = now / DAY_SECS;
        }
        if initialize {
            let (_, previous) = self.demand_at(now);
            self.funding_expected_claimants = previous.max(1);
            self.funding_reward_month = funding.month;
            self.funding_reward_pradha = Some(crate::reward_policy::pool_quota(
                funding.month,
                self.funding_daily_cap_pradha,
                self.funding_expected_claimants,
            ));
            self.funding_snapshot_dirty = true;
        }
        true
    }

    pub fn set_funding_address(&mut self, address: &str) -> Result<(), ClaimError> {
        let parsed = celenium_addresses::Address::try_from(address).map_err(|_| ClaimError::InvalidAddress)?;
        self.network_prefix = Some(parsed.prefix);
        Ok(())
    }

    pub fn invalidate_funding(&mut self) {
        self.funding = None;
        self.funding_sources = None;
    }

    pub fn service_reserve_pradha(&self) -> u64 {
        crate::reservoir_funds::service_reserve(
            self.peak_daily_spend_pradha.max(self.claimed_today().saturating_add(self.fees_today())),
        )
    }

    /// Fee on-chain yang sudah dibayar hari ini (0 bila sudah ganti hari).
    pub fn fees_today(&self) -> u64 {
        if Self::current_day() != self.fee_day { 0 } else { self.fees_today_pradha }
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        self.save_with_directory_sync(path, |dir| {
            #[cfg(unix)]
            {
                std::fs::File::open(dir)?.sync_all()
            }
            #[cfg(not(unix))]
            {
                let _ = dir;
                Ok(())
            }
        })
    }

    fn save_with_directory_sync(&mut self, path: &Path, sync_dir: impl FnOnce(&Path) -> std::io::Result<()>) -> std::io::Result<()> {
        use std::io::Write;
        let bytes = serde_json::to_vec_pretty(self)?;
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir)?;
        let temp = dir.join(format!(".faucet-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temp, path)?;
            // Rename sudah mengubah ledger; jangan membuang state ini jika fsync gagal.
            self.storage_uncertain = true;
            sync_dir(dir)?;
            self.storage_uncertain = false;
            self.funding_snapshot_dirty = false;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDR: &str = "cen69:qqh6qzvlczgp7nf7v7a0cwa9xqdskejse2575n7kwxa4vz778sarcqr5g90e6";
    const START: u64 = 1_794_787_200;

    #[test]
    fn dynamic_pool_claim_uses_cash_and_previous_day_demand() {
        let mut state = FaucetState::default();
        state.claim_day = START / DAY_SECS - 1;
        state.claimants_today = 10;
        state.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 3_000_000, 0, START);
        assert_eq!(state.claimable_at(ADDR, START), 300_000);
        assert_eq!(state.claim_at(ADDR, "device", START).unwrap().last_claim_payout_pradha, 300_000);
        assert_eq!(state.balance_response(ADDR, START)["rewardModel"], "daily-pool-halving");
        assert_eq!(state.claim_at(ADDR, "device", START), Err(ClaimError::AlreadyClaimed));
    }

    #[test]
    fn dynamic_pool_quota_survives_restart_and_midday_halving() {
        let mut state = FaucetState::default();
        state.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 3_000_000, 47, START);
        assert_eq!(state.claimable_at(ADDR, START), 500_000);
        let mut restored: FaucetState = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        restored.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 30_000_000, 48, START + 1);
        assert_eq!(restored.claimable_at(ADDR, START + 1), 500_000);
        restored.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 30_000_000, 48, START + DAY_SECS);
        assert_eq!(restored.claimable_at(ADDR, START + DAY_SECS), 250_000);
    }

    #[test]
    fn dynamic_pool_migration_preserves_today_quota_and_old_balances() {
        let fixture = serde_json::json!({
            "ledger": {ADDR: {"balance_pradha": 1000, "total_claimed_pradha": 1000,
                "last_claim_day": START / DAY_SECS, "daily_reward_pradha": 1000,
                "last_claim_reward_pradha": 1000, "last_claim_payout_pradha": 1000}},
            "devices": {}, "claim_day": START / DAY_SECS, "claimed_today": 1000
        });
        let mut state: FaucetState = serde_json::from_value(fixture).unwrap();
        let backing = crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 10_000_000;
        state.update_funding(backing, 0, START);
        assert_eq!(state.claim_at(ADDR, "device", START), Err(ClaimError::AlreadyClaimed));
        assert_eq!(state.balance(ADDR).unwrap().balance_pradha, 1000);
        state.update_funding(backing, 0, START + DAY_SECS);
        assert_eq!(state.claim_at(ADDR, "device", START + DAY_SECS).unwrap().balance_pradha, 501_000);
    }

    #[test]
    fn dynamic_pool_rejects_an_underfunded_minimum_without_credit() {
        let mut state = FaucetState::default();
        state.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 99_999, 0, START);
        assert_eq!(state.claimable_at(ADDR, START), 0);
        assert!(state.claim_at(ADDR, "device", START).is_err());
        assert_eq!(state.total_outstanding(), 0);
    }

    fn funded_state() -> FaucetState {
        let mut state = FaucetState::default();
        state.update_funding(
            crate::emission_policy::claim_bucket_pradha(0) + crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA,
            0,
            FaucetState::now_secs(),
        );
        state.funding_daily_cap_pradha = crate::emission_policy::daily_cap_pradha(0);
        state
    }

    impl FaucetState {
        fn funded_claim_at(&mut self, address: &str, device: &str, now: u64) -> Result<FaucetEntry, ClaimError> {
            self.update_funding(
                crate::emission_policy::claim_bucket_pradha(0) + crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA,
                0,
                now,
            );
            self.funding_daily_cap_pradha = crate::emission_policy::daily_cap_pradha(0);
            self.claim_at(address, device, now)
        }

        fn funded_claimable_at(&mut self, address: &str, now: u64) -> u64 {
            self.update_funding(
                crate::emission_policy::claim_bucket_pradha(0) + crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA,
                0,
                now,
            );
            self.funding_daily_cap_pradha = crate::emission_policy::daily_cap_pradha(0);
            self.claimable_at(address, now)
        }
    }

    #[test]
    fn reserve_cash_can_fund_claims_with_an_empty_operational_wallet() {
        use crate::reservoir_funds::{FaucetFundingSnapshot, FundingSnapshot};
        let mut state = FaucetState::default();
        let funds = FaucetFundingSnapshot {
            operational: FundingSnapshot { mature_pradha: 0, month: 0, observed_at: START },
            reserve: Some(FundingSnapshot { mature_pradha: FAUCET_MAX_OUTSTANDING_PRADHA, month: 0, observed_at: START }),
            reserve_configured: true,
        };
        assert!(state.update_funding_sources(funds, START));
        assert_eq!(state.claim_at(ADDR, "", START).unwrap().balance_pradha, 500_000);
        let mut missing = funds;
        missing.reserve = None;
        assert!(!state.update_funding_sources(missing, START));
        assert_eq!(state.claimable_at(ADDR, START), 0);
        assert_eq!(state.total_outstanding(), 500_000);
    }

    #[test]
    fn declining_reserve_target_preserves_legacy_claims_without_new_credit() {
        use crate::reservoir_funds::{FaucetFundingSnapshot, FundingSnapshot, OPERATIONAL_BUFFER_PRADHA};
        let mut state = FaucetState::default();
        let legacy = FAUCET_MAX_OUTSTANDING_PRADHA + 1000;
        state.ledger.insert(ADDR.into(), FaucetEntry { balance_pradha: legacy, ..Default::default() });
        let funds = FaucetFundingSnapshot {
            operational: FundingSnapshot { mature_pradha: 0, month: 816, observed_at: START },
            reserve: Some(FundingSnapshot { mature_pradha: legacy + OPERATIONAL_BUFFER_PRADHA, month: 816, observed_at: START }),
            reserve_configured: true,
        };
        assert!(state.update_funding_sources(funds, START));
        assert_eq!(state.funding.unwrap().mature_pradha, legacy + OPERATIONAL_BUFFER_PRADHA);
        assert_eq!(state.total_outstanding(), legacy);
        assert_eq!(state.claim_at(ADDR, "", START), Err(ClaimError::ReservoirFull));
    }

    #[test]
    fn unfunded_reservoir_cannot_issue_new_claims() {
        let mut state = FaucetState::default();
        assert!(state.claim_at(ADDR, "device", START).is_err(), "unfunded claims must be rejected");
        assert!(state.is_empty());
    }

    #[test]
    fn claims_cannot_cross_the_reservoir_network() {
        let mut state = funded_state();
        state.set_funding_address(ADDR).unwrap();
        let other =
            celenium_addresses::Address::new(celenium_addresses::Prefix::Simnet, celenium_addresses::Version::PubKey, &[1; 32]);
        assert_eq!(state.funded_claim_at(&other.to_string(), "device", START), Err(ClaimError::InvalidAddress));
        assert!(state.is_empty());
    }

    #[test]
    fn real_backing_survives_emission_end_without_erasing_old_claims() {
        let mut state = FaucetState::default();
        state.ledger.insert(ADDR.into(), FaucetEntry { balance_pradha: 5000, ..Default::default() });
        state.update_funding(crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 5000 + 365_000, 816, START);
        assert_eq!(state.claim_at(ADDR, "device", START), Err(ClaimError::FundingUnavailable));
        assert_eq!(state.balance(ADDR).unwrap().balance_pradha, 5000);
        state.update_funding(0, 816, START + DAY_SECS);
        assert_eq!(state.claim_at(ADDR, "device", START + DAY_SECS), Err(ClaimError::FundingUnavailable));
        assert_eq!(state.balance(ADDR).unwrap().balance_pradha, 5000);
    }

    #[test]
    fn stale_backing_cannot_issue_credit() {
        let mut state = funded_state();
        state.update_funding(u64::MAX, 0, START);
        assert_eq!(state.claim_at(ADDR, "device", START + 31), Err(ClaimError::FundingUnavailable));
    }

    #[test]
    fn confirmed_withdrawal_is_debited_once_after_journal_replay() {
        let mut state = funded_state();
        state.ledger.insert(ADDR.into(), FaucetEntry { balance_pradha: 5000, ..Default::default() });
        assert!(state.settle_withdrawal("test-tx", ADDR, 1000, 500));
        let mut restored: FaucetState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(restored.settle_withdrawal("test-tx", ADDR, 1000, 500));
        assert_eq!(restored.balance(ADDR).unwrap().balance_pradha, 4000);
        assert_eq!(restored.fees_today_pradha, 500);
    }

    #[test]
    fn cash_budget_cannot_increase_with_each_claim_or_restart() {
        let mut state = FaucetState::default();
        let backing = crate::reservoir_funds::OPERATIONAL_BUFFER_PRADHA + 1_000_000;
        state.update_funding(backing, 0, START);
        let cap = state.daily_cap_at(START);
        state.claim_at(ADDR, "device", START).unwrap();
        for tick in 1..30 {
            state.update_funding(backing, 0, START + tick);
            assert_eq!(state.daily_cap_at(START + tick), cap);
        }
        let mut restored: FaucetState = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        restored.update_funding(backing, 0, START + 30);
        assert_eq!(restored.daily_cap_at(START + 30), cap);
    }

    #[test]
    fn pool_daily_claim_pays_the_frozen_quota_at_any_time_of_day() {
        for elapsed in [0, 1, 1800, 43_200, 86_399] {
            let mut s = funded_state();
            let entry = s.funded_claim_at(ADDR, "device", START + elapsed).unwrap();
            assert_eq!(entry.last_claim_payout_pradha, 500_000);
            assert_eq!(entry.balance_pradha, 500_000);
            assert_eq!(s.claimed_today, 500_000);
            assert_eq!(s.claimants_today, 1);
            assert!(s.funded_claim_at(ADDR, "device", START + elapsed).is_err());
            assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 500_000);
        }
    }

    #[test]
    fn fixed_daily_claim_tops_up_existing_partial_drip_once() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimed_today = 20;
        s.claimants_today = 1;
        s.ledger.insert(
            ADDR.into(),
            FaucetEntry {
                balance_pradha: 20,
                total_claimed_pradha: 20,
                last_claim_day: START / DAY_SECS,
                last_claim_reward_pradha: 20,
                daily_reward_pradha: 1000,
                last_claim_payout_pradha: 20,
                last_withdraw_secs: 0,
            },
        );
        assert_eq!(s.funded_claimable_at(ADDR, START + 1800), 980);
        let entry = s.funded_claim_at(ADDR, "device", START + 1800).unwrap();
        assert_eq!(entry.balance_pradha, 1000);
        assert_eq!(entry.last_claim_payout_pradha, 980);
        assert_eq!(s.claimants_today, 1);
        assert!(s.funded_claim_at(ADDR, "device", START + 3600).is_err());
    }

    #[test]
    fn dynamic_pool_reward_has_a_funded_minimum_when_demand_is_large() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.last_day_claimants = 30;
        s.funding_budget_day = START / DAY_SECS;
        s.funding_daily_cap_pradha = 3_000_000;
        s.funding_reward_pradha = None;
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 1800).unwrap().last_claim_payout_pradha, 100_000);
    }

    #[test]
    fn fixed_daily_claim_rejects_short_budget_without_partial_credit() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimed_today = crate::emission_policy::daily_cap_pradha(0) - 999;
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 43_200), Err(ClaimError::DailyCapReached));
        assert!(s.is_empty());
    }

    #[test]
    fn preview_stops_when_daily_budget_is_exhausted() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimed_today = crate::emission_policy::daily_cap_pradha(0);
        assert_eq!(s.funded_claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 43_200), Err(ClaimError::DailyCapReached));
    }

    #[test]
    fn preview_stops_when_liability_cap_is_full() {
        let mut s = funded_state();
        s.ledger
            .insert("existing-liability".into(), FaucetEntry { balance_pradha: FAUCET_MAX_OUTSTANDING_PRADHA, ..Default::default() });
        assert_eq!(s.funded_claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 43_200), Err(ClaimError::ReservoirFull));
    }

    #[test]
    fn legacy_daily_claim_is_not_paid_again() {
        let fixture = serde_json::json!({
            "ledger": {ADDR: {"balance_pradha": 1000, "total_claimed_pradha": 1000, "last_claim_day": START / DAY_SECS}},
            "devices": {}, "claim_day": START / DAY_SECS, "claimed_today": 1000
        });
        let mut s: FaucetState = serde_json::from_value(fixture).unwrap();
        assert_eq!(s.funded_claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 43_200), Err(ClaimError::AlreadyClaimed));
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 1000);
    }

    #[test]
    fn failed_claim_does_not_register_a_device() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimed_today = crate::emission_policy::daily_cap_pradha(0);
        assert_eq!(s.funded_claim_at(ADDR, "device", START), Err(ClaimError::DailyCapReached));
        assert!(s.devices.is_empty());
    }

    #[test]
    fn padded_address_is_rejected_instead_of_creating_another_account() {
        let mut s = funded_state();
        let address = format!(" {ADDR} ");
        assert_eq!(s.funded_claim_at(&address, "device", START + 43_200), Err(ClaimError::InvalidAddress));
        assert!(s.is_empty());
    }

    #[test]
    fn preview_matches_claim_at_day_rollover_with_changing_demand() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimants_today = 10;
        let now = START + DAY_SECS + 43_200;
        let preview = s.funded_claimable_at(ADDR, now);
        let claim = s.funded_claim_at(ADDR, "device", now).unwrap();
        assert_eq!(preview, claim.last_claim_payout_pradha);
    }

    #[test]
    fn previous_drip_remainder_is_preserved_with_new_daily_claim() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.into(),
            FaucetEntry {
                balance_pradha: 500,
                total_claimed_pradha: 500,
                last_claim_day: START / DAY_SECS,
                last_claim_reward_pradha: 500,
                daily_reward_pradha: 1000,
                ..Default::default()
            },
        );
        let next = s.funded_claim_at(ADDR, "device", START + DAY_SECS).unwrap();
        assert_eq!(next.last_claim_payout_pradha, 500_500);
        assert_eq!(next.balance_pradha, 501_000);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + DAY_SECS), Err(ClaimError::AlreadyClaimed));
    }

    #[test]
    fn corrupt_ledger_is_not_loaded_as_an_empty_balance() {
        let path = std::env::temp_dir().join(format!("faucet-corrupt-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"{broken").unwrap();
        let error = FaucetState::load(&path).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn save_can_be_reloaded_after_claim_without_double_credit() {
        let path = std::env::temp_dir().join(format!("faucet-fixed-{}.json", uuid::Uuid::new_v4()));
        let mut s = funded_state();
        s.funded_claim_at(ADDR, "device", START + 1800).unwrap();
        s.save(&path).unwrap();
        let mut loaded = FaucetState::load(&path).unwrap();
        assert_eq!(loaded.funded_claim_at(ADDR, "device", START + 3600), Err(ClaimError::AlreadyClaimed));
        assert_eq!(loaded.funded_claim_at(ADDR, "device", START + DAY_SECS).unwrap().balance_pradha, 1_000_000);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn directory_sync_failure_retains_credit_and_blocks_mutations() {
        let path = std::env::temp_dir().join(format!("faucet-sync-{}.json", uuid::Uuid::new_v4()));
        let mut s = funded_state();
        s.funded_claim_at(ADDR, "device", START + 1800).unwrap();
        let result = s.save_with_directory_sync(&path, |_| Err(std::io::Error::other("injected directory sync failure")));
        assert!(result.is_err());
        assert!(s.storage_uncertain);
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 500_000);
        assert_eq!(FaucetState::load(&path).unwrap().balance(ADDR).unwrap().balance_pradha, 500_000);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 3600), Err(ClaimError::StorageUnavailable));
        assert_eq!(s.check_withdraw(ADDR), Err(WithdrawError::StorageUnavailable));
        s.save(&path).unwrap();
        assert!(!s.storage_uncertain);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + DAY_SECS).unwrap().balance_pradha, 1_000_000);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn zero_credit_legacy_drip_enrollment_keeps_its_original_entitlement() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.claimants_today = 1;
        s.ledger.insert(ADDR.into(), FaucetEntry { last_claim_day: START / DAY_SECS, daily_reward_pradha: 1, ..Default::default() });
        s.funded_claimable_at(ADDR, START);
        let response = s.balance_response(ADDR, START);
        assert_eq!(response["canStartDrip"], false);
        assert_eq!(response["rewardModel"], "daily-pool-halving");
        assert_eq!(response["rewardPradha"], 1);
        assert_eq!(s.funded_claim_at(ADDR, "device", START).unwrap().balance_pradha, 1);
        assert_eq!(s.claimants_today, 1);
    }

    #[test]
    fn pool_claim_is_available_at_midnight_without_waiting() {
        let mut s = funded_state();
        s.funded_claimable_at(ADDR, START);
        assert_eq!(s.balance_response(ADDR, START)["rewardPradha"], 500_000);
        assert_eq!(s.funded_claim_at(ADDR, "device", START).unwrap().balance_pradha, 500_000);
    }

    #[test]
    fn pool_claim_is_once_per_day_and_returns_next_day() {
        let mut s = funded_state();
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 1).unwrap().balance_pradha, 500_000);
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 43_200), Err(ClaimError::AlreadyClaimed));
        assert_eq!(s.funded_claim_at(ADDR, "device", START + DAY_SECS - 1), Err(ClaimError::AlreadyClaimed));
        assert_eq!(s.funded_claim_at(ADDR, "device", START + DAY_SECS).unwrap().balance_pradha, 1_000_000);
        assert_eq!(s.claimants_today, 1);
    }

    #[test]
    fn withdrawal_does_not_make_the_daily_reward_claimable_again() {
        let mut s = funded_state();
        s.funded_claim_at(ADDR, "device", START).unwrap();
        assert!(s.debit(ADDR, 500_000));
        assert_eq!(s.funded_claim_at(ADDR, "device", START + 1800), Err(ClaimError::AlreadyClaimed));
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 0);
    }

    #[test]
    fn existing_claimed_balance_survives_day_rollover() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: 700,
                last_claim_day: 10,
                total_claimed_pradha: 700,
                last_claim_reward_pradha: 700,
                daily_reward_pradha: 1000,
                last_claim_payout_pradha: 700,
                last_withdraw_secs: 0,
            },
        );
        let next = s.funded_claim_at(ADDR, "", 11 * DAY_SECS + DAY_SECS / 2).unwrap();
        assert_eq!(next.balance_pradha, 501_000);
        assert_eq!(next.last_claim_reward_pradha, 500_000);
        assert_eq!(next.daily_reward_pradha, 500_000);
    }

    #[test]
    fn rejects_empty_address() {
        let mut s = funded_state();
        assert_eq!(s.claim("", ""), Err(ClaimError::EmptyAddress));
        assert_eq!(s.claim("   ", ""), Err(ClaimError::EmptyAddress));
    }

    #[test]
    fn debit_deducts_balance() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: 5000,
                last_claim_day: 0,
                total_claimed_pradha: 5000,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        assert!(s.debit(ADDR, 3000));
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 2000);
        assert!(!s.debit(ADDR, 9999));
    }

    #[test]
    fn total_outstanding_tracks_ledger() {
        let mut s = funded_state();
        s.ledger.insert(
            "a".into(),
            FaucetEntry {
                balance_pradha: 1000,
                last_claim_day: 0,
                total_claimed_pradha: 1000,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        s.ledger.insert(
            "b".into(),
            FaucetEntry {
                balance_pradha: 2000,
                last_claim_day: 0,
                total_claimed_pradha: 2000,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        assert_eq!(s.total_outstanding(), 3000);
    }

    #[test]
    fn daily_cap_stops_issuance() {
        let mut s = funded_state();
        s.claim_day = FaucetState::current_day();
        // Cap harian dinamis: isi tepat sampai cap, klaim berikutnya harus ditolak.
        s.claimed_today = s.daily_cap_now();
        assert!(s.daily_cap_now() > 0, "cap dinamis harus > 0 di fase emisi");
        assert_eq!(s.claim(ADDR, ""), Err(ClaimError::DailyCapReached));
    }

    #[test]
    fn device_cap_limits_accounts() {
        let mut s = funded_state();
        // Daftarkan 5 address berbeda pada device yang sama.
        for i in 0..FAUCET_MAX_ACCOUNTS_PER_DEVICE {
            let a = [
                "cen69:qr7nlqwkawksc5h43xglnrr40w36a0lu4fr5fd7swqerxee02yddz6kq5cpxa",
                "cen69:qzczdkrh98g65gl7u2h0vcjxtszzlyk3rlykn0ejr3zkvq7dg247qk4slnyrz",
                "cen69:qr65k3j4yrnajp4kus7duvkwwp9m4wddqwpp307e029ek0g840hmw5qdrs424",
                "cen69:qzlwk2rtjvp5skpj7ywmk9e9qw7gq4hnpmxeu2d9jff7zwz278k55qxmdklnt",
                "cen69:qptdhd3arqvleucg3rtrswq9grpznk7cqnz3nv39467pr4xt7g2kkwx6fcz42",
            ][i];
            s.ledger.insert(
                a.to_string(),
                FaucetEntry {
                    balance_pradha: 0,
                    last_claim_day: 0,
                    total_claimed_pradha: 0,
                    last_claim_reward_pradha: 0,
                    daily_reward_pradha: 0,
                    last_claim_payout_pradha: 0,
                    last_withdraw_secs: 0,
                },
            );
            s.devices.entry("dev-1".to_string()).or_default().insert(a.to_string());
        }
        // Address ke-6 ditolak.
        let sixth = "cen69:qqjc2n5jqed3z5s3nsk4cmeqf3zf2adt8l9my3jdn3kguk3pwle35ue4hkr67";
        assert_eq!(s.claim(sixth, "dev-1"), Err(ClaimError::TooManyAccounts));
        // Device lain masih diizinkan.
        assert!(s.funded_claim_at(sixth, "dev-2", START + 43_200).is_ok());
    }

    #[test]
    fn save_load_roundtrip() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: 7000,
                last_claim_day: 5,
                total_claimed_pradha: 7000,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        let p = std::env::temp_dir().join("faucet_test_roundtrip.json");
        s.save(&p).unwrap();
        let loaded = FaucetState::load(&p).unwrap();
        assert_eq!(loaded.balance(ADDR).unwrap().balance_pradha, 7000);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_invalid_address() {
        let mut s = funded_state();
        assert_eq!(s.claim("not-an-address", "dev-x"), Err(ClaimError::InvalidAddress));
        assert!(
            s.funded_claim_at("cen69:qzsgej828lu39wkvqfq8d2hwnul9ru79g3ja4ulj4y5cer25reym2unh2sjmw", "dev-x", START + 43_200).is_ok()
        );
    }

    #[test]
    fn outstanding_cap_stops_issuance() {
        let mut s = funded_state();
        let now = START + 43_200;
        let cap = FAUCET_MAX_OUTSTANDING_PRADHA;
        s.ledger.insert(
            "cen69:qzsgej828lu39wkvqfq8d2hwnul9ru79g3ja4ulj4y5cer25reym2unh2sjmw".into(),
            FaucetEntry {
                balance_pradha: cap - 500 + 1,
                last_claim_day: 0,
                total_claimed_pradha: cap - 500 + 1,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        assert_eq!(s.funded_claim_at(ADDR, "", now), Err(ClaimError::ReservoirFull));
    }

    #[test]
    fn outstanding_claims_are_capped_at_the_faucet_reserve_limit() {
        let mut s = FaucetState::default();
        s.update_funding(u64::MAX, 0, START);

        assert_eq!(s.outstanding_cap_at(START), 500_000 * 100_000_000);
        s.update_funding(u64::MAX, 816, START);
        assert_eq!(s.outstanding_cap_at(START), 500_000 * 100_000_000);
    }

    #[test]
    fn lowering_floor_never_erases_existing_faucet_claims() {
        let mut s = FaucetState::default();
        let existing = 600_000 * 100_000_000;
        s.ledger.insert(ADDR.into(), FaucetEntry { balance_pradha: existing, ..Default::default() });
        s.update_funding(u64::MAX, 816, START);

        assert_eq!(s.outstanding_cap_at(START), existing);
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, existing);
    }

    #[test]
    fn fully_backed_claims_stop_at_five_hundred_thousand_cen() {
        let mut s = FaucetState::default();
        let limit = 500_000 * 100_000_000;
        s.ledger.insert(ADDR.into(), FaucetEntry { balance_pradha: limit - 500_000, ..Default::default() });
        let now = FaucetState::now_secs();
        s.update_funding(u64::MAX, 0, now);
        assert_eq!(s.claim_at(ADDR, "", now).unwrap().balance_pradha, limit);
        let before = serde_json::to_value(&s).unwrap();
        assert_eq!(s.reward_pradha_today(), 0);
        assert_eq!(s.claim_at(ADDR, "", now + DAY_SECS), Err(ClaimError::FundingUnavailable));
        s.update_funding(u64::MAX, 0, now + DAY_SECS);
        assert_eq!(s.claim_at(ADDR, "", now + DAY_SECS), Err(ClaimError::ReservoirFull));
        assert_eq!(serde_json::to_value(&s).unwrap()["ledger"], before["ledger"]);
    }

    #[test]
    fn withdraw_fee_check_rejects_when_actual_fee_exceeds_remaining_budget() {
        let mut s = funded_state();
        s.fee_day = FaucetState::current_day();
        s.fees_today_pradha = FAUCET_DAILY_FEE_BUDGET_PRADHA - 500;
        assert!(s.check_withdraw_fee(ADDR, 500).is_ok());
        assert!(matches!(s.check_withdraw_fee(ADDR, 501), Err(WithdrawError::FeeBudgetExhausted { .. })));
        assert_eq!(s.fees_today_pradha, FAUCET_DAILY_FEE_BUDGET_PRADHA - 500);
    }

    #[test]
    fn withdraw_cooldown_blocks_repeat() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: 1000,
                last_claim_day: 0,
                total_claimed_pradha: 1000,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        assert!(s.check_withdraw(ADDR).is_ok());
        s.record_withdraw(ADDR, 100, 25).then_some(()).expect("record should succeed");
        assert!(matches!(s.check_withdraw(ADDR), Err(WithdrawError::Cooldown { .. })));
        // Saldo terdebit, last_withdraw_secs terisi, fee tercatat.
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 900);
        assert_eq!(s.balance(ADDR).unwrap().last_withdraw_secs, FaucetState::now_secs());
    }

    #[test]
    fn fee_budget_exhausts_after_cap() {
        let mut s = funded_state();
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: u64::MAX >> 1,
                last_claim_day: 0,
                total_claimed_pradha: 0,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        for _ in 0..(FAUCET_DAILY_FEE_BUDGET_PRADHA / 25) {
            s.record_withdraw(ADDR, 1, 25);
        }
        assert!(matches!(s.check_withdraw(ADDR), Err(WithdrawError::FeeBudgetExhausted { .. })));
    }

    #[test]
    fn minimum_withdraw_is_reachable_by_accumulating_claims() {
        assert_eq!(FAUCET_MIN_WITHDRAW_PRADHA, 2_500_000);
        assert_eq!(FAUCET_MIN_WITHDRAW_PRADHA.div_ceil(FAUCET_MAX_REWARD_PRADHA), 5);
        assert_eq!(FAUCET_MIN_WITHDRAW_PRADHA.div_ceil(FAUCET_MIN_REWARD_PRADHA), 25);
    }

    #[test]
    fn frozen_pool_reward_does_not_change_after_new_participants_arrive() {
        let mut s = funded_state();
        s.claim_day = FaucetState::current_day();
        for participants in [0, 200_000_000, 2_025_000_000, u64::MAX] {
            s.last_day_claimants = participants;
            assert_eq!(s.reward_pradha_today(), 500_000);
        }
        s.claimed_today = s.daily_cap_now() - 999;
        assert_eq!(s.reward_pradha_today(), 0);
    }

    #[test]
    fn daily_payout_never_exceeds_cap_under_stress() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        let cap = crate::emission_policy::daily_cap_pradha(0);
        s.claimed_today = cap - 2_750_000;
        let initial = s.claimed_today;
        let mut paid = 0;
        let mut successes = 0;
        for i in 0..256u64 {
            let mut payload = [0u8; 32];
            payload[..8].copy_from_slice(&i.to_le_bytes());
            let address = celenium_addresses::Address::try_new(
                celenium_addresses::Prefix::Mainnet,
                celenium_addresses::Version::PubKey,
                &payload,
            )
            .unwrap()
            .to_string();
            if let Ok(entry) = s.funded_claim_at(&address, "", START + 43_200) {
                paid += entry.last_claim_payout_pradha;
                successes += 1;
            }
            assert!(s.claimed_today <= cap);
        }
        assert!(successes > 0);
        assert!(paid > 0 && paid <= 2_750_000);
        assert_eq!(s.claimed_today, initial + paid);
        assert_eq!(s.total_outstanding(), paid);
    }

    /// Setelah anggaran harian habis, klaim baru harus ditolak — bukan
    /// membayar dengan nilai sembarang.
    #[test]
    fn exhausted_budget_rejects_further_claims() {
        let mut s = funded_state();
        s.claim_day = FaucetState::current_day();
        s.claimed_today = s.daily_cap_now();
        assert_eq!(s.reward_pradha_today(), 0);
        assert_eq!(s.claim(ADDR, ""), Err(ClaimError::DailyCapReached));
    }

    /// Reward yang dilaporkan API harus sama dengan yang benar-benar dibayar.
    #[test]
    fn reported_reward_matches_actual_payout() {
        let mut s = funded_state();
        s.claim_day = START / DAY_SECS;
        s.last_day_claimants = 500_000;
        let now = START + 43_200;
        let before = s.funded_claimable_at(ADDR, now);
        let entry = s.funded_claim_at(ADDR, "", now).unwrap();
        assert_eq!(entry.last_claim_payout_pradha, before);
        assert_eq!(entry.last_claim_reward_pradha, before);
        assert_eq!(s.balance_response(ADDR, now)["rewardPradha"], 0);
    }
}
