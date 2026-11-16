//! Faucet klaim reward bertahap: buku besar off-chain partisipasi.
//!
//! Setiap alamat punya kuota harian dinamis dengan plafon
//! `FAUCET_MAX_REWARD_PRADHA`. Kuota itu terakru linear dari pukul 00:00 sampai
//! 24:00 UTC, dan klaim berulang hanya mengambil akrual yang belum dibayar.
//! Saldo tetap off-chain sampai user menariknya on-chain; lihat
//! `docs/faucet-reservoir-spec.md`.
//!
//! Akuntansi memakai pradha (1 CEN = 1e8 pradha) agar integer, tanpa float.

use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Hadiah partisipasi **maksimum** per klaim harian (pradha; 1 CEN = 1e8 pradha).
///
/// 1000 pradha = 0,00001 CEN per perangkat per hari. Ini plafon, bukan janji:
/// reward yang benar-benar dibayar menyusut ketika peserta bertambah, karena
/// anggaran harian dibagi rata ke jumlah peserta yang terukur
/// (lihat [`FaucetState::reward_pradha_today`]).
pub const FAUCET_MAX_REWARD_PRADHA: u64 = 1_000;

/// Batas total kewajiban ledger (semua saldo yang belum ditarik), dalam pradha.
/// Cap harian hanya membatasi emisi per hari; tanpa batas akumulatif, kewajiban
/// menumpuk selamanya dan faucet berutang lebih besar daripada reservoir yang
/// menopangnya. Nilai ini = 2.500.000 CEN, sama dengan target reservoir: klaim
/// baru berhenti begitu total saldo tercatat mencapai titik di mana reservoir
/// seharusnya sudah penuh.
///
/// Ini plafon keras keselamatan. Batas operasional sehari-hari adalah floor
/// dinamis ([`crate::emission_policy::floor_pradha`]), yang menyusut mengikuti
/// sisa emisi sehingga kewajiban tidak pernah menumpuk melebihi dana yang bisa
/// datang.
pub const FAUCET_MAX_OUTSTANDING_PRADHA: u64 = 250_000_000_000_000; // 2.500.000 CEN

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

/// Nominal withdraw minimum, dalam pradha (0,00001 CEN).
///
/// Disamakan dengan plafon klaim harian, agar saldo satu klaim penuh dapat
/// ditarik. Penarikan tetap tunduk pada cooldown per alamat dan anggaran fee
/// harian; fee jaringan tetap dibayar reservoir.
pub const FAUCET_MIN_WITHDRAW_PRADHA: u64 = 1_000;

/// Reward minimum per klaim, dalam pradha.
///
/// Pradha adalah satuan terkecil, jadi 1 adalah lantai mutlak. Nilai ini hanya
/// terpakai ketika anggaran harian lebih kecil daripada jumlah peserta: pada
/// titik itu yang dibatasi adalah **jumlah klaim terlayani**, bukan nilai per
/// klaim.
pub const FAUCET_MIN_REWARD_PRADHA: u64 = 1;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// Belum ada porsi yang terakru sejak 00:00 UTC, atau porsi hari ini
    /// seluruhnya sudah diambil.
    NothingAccrued,
    DailyCapReached,
    ReservoirFull,
    InvalidAddress,
    EmptyAddress,
    TooManyAccounts,
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ClaimError::NothingAccrued => "no reward accrued yet; it drips in over 24h from 00:00 UTC",
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
            Self::NothingAccrued => "NothingAccrued",
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
    /// Alamat menarik lagi sebelum jeda antar-withdraw lewat.
    Cooldown { remaining_secs: u64 },
    /// Anggaran fee on-chain hari ini sudah habis.
    FeeBudgetExhausted { used_pradha: u64, budget_pradha: u64 },
}

impl std::fmt::Display for WithdrawError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
        if now / DAY_SECS == self.claim_day { (self.claimants_today, self.last_day_claimants) } else { (0, self.claimants_today) }
    }

    pub fn expected_claimants_today(&self) -> u64 {
        let (current, previous) = self.demand_at(Self::now_secs());
        current.max(previous).max(1)
    }

    /// Kuota penuh untuk alamat baru; akrual dihitung terpisah.
    pub fn reward_pradha_today(&self) -> u64 {
        self.new_daily_quota_at(Self::now_secs())
    }

    fn new_daily_quota_at(&self, now: u64) -> u64 {
        let day = now / DAY_SECS;
        let month = crate::emission_policy::month_since_genesis(now);
        let cap = crate::emission_policy::daily_cap_pradha(month);
        let used = if day == self.claim_day { self.claimed_today } else { 0 };
        let remaining = cap.saturating_sub(used);
        if remaining == 0 {
            return 0;
        }
        let (current, previous) = self.demand_at(now);
        let slots = current.max(previous).max(1).saturating_sub(current).max(1);
        (remaining / slots).clamp(FAUCET_MIN_REWARD_PRADHA, FAUCET_MAX_REWARD_PRADHA)
    }

    fn daily_quota_for(&self, address: &str, now: u64) -> u64 {
        if let Some(entry) = self.balance(address)
            && entry.last_claim_day == now / DAY_SECS
        {
            // Ledger lama membayar seluruh kuota dalam satu klaim.
            return if entry.daily_reward_pradha > 0 { entry.daily_reward_pradha } else { entry.last_claim_reward_pradha };
        }
        self.new_daily_quota_at(now)
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
        let quota = self.daily_quota_for(address, now);
        let entry = self.balance(address);
        let paid = entry
            .filter(|e| e.last_claim_day == day)
            .map(|e| if e.daily_reward_pradha == 0 { quota } else { e.last_claim_reward_pradha })
            .unwrap_or(0);
        let pending = entry
            .filter(|e| e.last_claim_day < day && e.daily_reward_pradha > 0)
            .map(|e| e.daily_reward_pradha.saturating_sub(e.last_claim_reward_pradha))
            .unwrap_or(0);
        let accrued = (u128::from(quota) * u128::from(now % DAY_SECS) / u128::from(DAY_SECS)) as u64;
        ClaimAmounts { quota, accrued, paid, pending, payout: pending.saturating_add(accrued.saturating_sub(paid)) }
    }

    fn check_payout_at(&self, address: &str, now: u64, payout: u64) -> Result<(), ClaimError> {
        Self::validate_address(address)?;
        let day = now / DAY_SECS;
        // Jangan membuka ulang hari yang sudah dilewati setelah koreksi jam server.
        if day < self.claim_day || self.balance(address).is_some_and(|e| e.last_claim_day > day) {
            return Err(ClaimError::NothingAccrued);
        }
        let month = crate::emission_policy::month_since_genesis(now);
        let daily_cap = crate::emission_policy::daily_cap_pradha(month);
        let used = if self.claim_day == day { self.claimed_today } else { 0 };
        if used >= daily_cap || payout > daily_cap.saturating_sub(used) {
            return Err(ClaimError::DailyCapReached);
        }
        let cap = crate::emission_policy::claim_bucket_pradha(month).min(FAUCET_MAX_OUTSTANDING_PRADHA);
        let outstanding = self.total_outstanding();
        if outstanding >= cap || payout > cap.saturating_sub(outstanding) {
            return Err(ClaimError::ReservoirFull);
        }
        if payout == 0 {
            return Err(ClaimError::NothingAccrued);
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

    fn can_start_drip_at(&self, address: &str, now: u64) -> bool {
        self.daily_quota_for(address, now) == FAUCET_MIN_REWARD_PRADHA
            && self.balance(address).is_none_or(|e| e.last_claim_day < now / DAY_SECS)
            && self.check_payout_at(address, now, FAUCET_MIN_REWARD_PRADHA).is_ok()
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
            "canStartDrip": reward == 0 && self.can_start_drip_at(address, now),
            "serverTimeSecs": now,
            "dripDurationSecs": DAY_SECS,
            "dripModel": "linear-utc-day",
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
        if !(amounts.payout == 0 && self.can_start_drip_at(address, now)) {
            self.check_payout_at(address, now, amounts.payout)?;
        }
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
        true
    }

    /// Kurangi saldo (dipanggil setelah withdraw on-chain berhasil).
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
        let month = crate::emission_policy::month_since_genesis(Self::now_secs());
        crate::emission_policy::daily_cap_pradha(month)
    }

    /// Floor/kewajiban maksimum dinamis saat ini (pradha), dibatasi plafon keras.
    ///
    /// Memakai bucket klaim, bukan floor penuh — porsi miner Android bukan
    /// kewajiban faucet.
    pub fn outstanding_cap_now(&self) -> u64 {
        let month = crate::emission_policy::month_since_genesis(Self::now_secs());
        crate::emission_policy::claim_bucket_pradha(month).min(FAUCET_MAX_OUTSTANDING_PRADHA)
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

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
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
            // Setelah rename, memori harus mengikuti ledger yang telah terpasang.
            #[cfg(unix)]
            if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
                tracing::warn!("faucet directory sync failed after ledger replacement: {e}");
            }
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
    fn preview_stops_when_daily_budget_is_exhausted() {
        let mut s = FaucetState::default();
        s.claim_day = START / DAY_SECS;
        s.claimed_today = crate::emission_policy::daily_cap_pradha(0);
        assert_eq!(s.claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.claim_at(ADDR, "device", START + 43_200), Err(ClaimError::DailyCapReached));
    }

    #[test]
    fn preview_stops_when_liability_cap_is_full() {
        let mut s = FaucetState::default();
        s.ledger.insert(
            "existing-liability".into(),
            FaucetEntry { balance_pradha: crate::emission_policy::claim_bucket_pradha(0), ..Default::default() },
        );
        assert_eq!(s.claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.claim_at(ADDR, "device", START + 43_200), Err(ClaimError::ReservoirFull));
    }

    #[test]
    fn legacy_daily_claim_is_not_paid_again() {
        let fixture = serde_json::json!({
            "ledger": {ADDR: {"balance_pradha": 1000, "total_claimed_pradha": 1000, "last_claim_day": START / DAY_SECS}},
            "devices": {}, "claim_day": START / DAY_SECS, "claimed_today": 1000
        });
        let mut s: FaucetState = serde_json::from_value(fixture).unwrap();
        assert_eq!(s.claimable_at(ADDR, START + 43_200), 0);
        assert_eq!(s.claim_at(ADDR, "device", START + 43_200), Err(ClaimError::NothingAccrued));
        assert_eq!(s.balance(ADDR).unwrap().balance_pradha, 1000);
    }

    #[test]
    fn failed_claim_does_not_register_a_device() {
        let mut s = FaucetState::default();
        assert_eq!(s.claim_at(ADDR, "device", START), Err(ClaimError::NothingAccrued));
        assert!(s.devices.is_empty());
    }

    #[test]
    fn padded_address_is_rejected_instead_of_creating_another_account() {
        let mut s = FaucetState::default();
        let address = format!(" {ADDR} ");
        assert_eq!(s.claim_at(&address, "device", START + 43_200), Err(ClaimError::InvalidAddress));
        assert!(s.is_empty());
    }

    #[test]
    fn preview_matches_claim_at_day_rollover_with_changing_demand() {
        let mut s = FaucetState::default();
        s.claim_day = START / DAY_SECS;
        s.claimants_today = 200_000_000;
        let now = START + DAY_SECS + 43_200;
        let preview = s.claimable_at(ADDR, now);
        let claim = s.claim_at(ADDR, "device", now).unwrap();
        assert_eq!(preview, claim.last_claim_payout_pradha);
    }

    #[test]
    fn partial_claim_reaches_daily_quota_after_midnight() {
        let mut s = FaucetState::default();
        assert_eq!(s.claim_at(ADDR, "device", START + 43_200).unwrap().balance_pradha, 500);
        let next = s.claim_at(ADDR, "device", START + DAY_SECS).unwrap();
        assert_eq!(next.last_claim_payout_pradha, 500);
        assert_eq!(next.balance_pradha, 1000);
        assert_eq!(s.claim_at(ADDR, "device", START + DAY_SECS), Err(ClaimError::NothingAccrued));
        let later = s.claim_at(ADDR, "device", START + DAY_SECS + 1800).unwrap();
        assert_eq!(later.last_claim_payout_pradha, 20);
        assert_eq!(later.balance_pradha, 1020);
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
    fn save_can_be_reloaded_after_partial_claim_without_double_credit() {
        let path = std::env::temp_dir().join(format!("faucet-drip-{}.json", uuid::Uuid::new_v4()));
        let mut s = FaucetState::default();
        s.claim_at(ADDR, "device", START + 1800).unwrap();
        s.save(&path).unwrap();
        let mut loaded = FaucetState::load(&path).unwrap();
        assert_eq!(loaded.claim_at(ADDR, "device", START + 1800), Err(ClaimError::NothingAccrued));
        assert_eq!(loaded.claim_at(ADDR, "device", START + 3600).unwrap().balance_pradha, 41);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn minimum_daily_quota_can_start_without_inventing_credit() {
        let mut s = FaucetState::default();
        s.claim_day = START / DAY_SECS;
        s.last_day_claimants = u64::MAX;
        let response = s.balance_response(ADDR, START + 43_200);
        assert_eq!(response["canStartDrip"], true);
        assert_eq!(response["canClaimToday"], false);
        let enrolled = s.claim_at(ADDR, "device", START + 43_200).unwrap();
        assert_eq!(enrolled.daily_reward_pradha, 1);
        assert_eq!(enrolled.balance_pradha, 0);
        assert_eq!(s.claimants_today, 1);
        assert_eq!(s.claim_at(ADDR, "device", START + 43_200), Err(ClaimError::NothingAccrued));
        assert_eq!(s.claim_at(ADDR, "device", START + DAY_SECS).unwrap().last_claim_payout_pradha, 1);
    }

    #[test]
    fn no_credit_before_one_whole_pradha_accrues() {
        let mut s = FaucetState::default();
        let day = START / DAY_SECS;
        s.ledger.insert(
            ADDR.to_string(),
            FaucetEntry {
                balance_pradha: 0,
                last_claim_day: day.saturating_sub(1),
                total_claimed_pradha: 0,
                last_claim_reward_pradha: 0,
                daily_reward_pradha: 0,
                last_claim_payout_pradha: 0,
                last_withdraw_secs: 0,
            },
        );
        assert_eq!(s.claim_at(ADDR, "", day * DAY_SECS + 1), Err(ClaimError::NothingAccrued));
    }

    #[test]
    fn claims_accumulate_linearly_and_can_be_repeated() {
        let mut s = FaucetState::default();
        let day = START / DAY_SECS;
        s.claim_day = day;
        let quota = s.daily_quota_for(ADDR, day * DAY_SECS + DAY_SECS / 2);
        assert!(quota > 0);
        assert_eq!(s.claimable_at(ADDR, day * DAY_SECS + DAY_SECS / 2), quota / 2);
        let first = s.claim_at(ADDR, "", day * DAY_SECS + DAY_SECS / 4).unwrap();
        assert_eq!(first.balance_pradha, quota / 4);
        assert_eq!(first.last_claim_payout_pradha, quota / 4);
        assert_eq!(s.claimable_at(ADDR, day * DAY_SECS + DAY_SECS / 2), quota / 4);
        let second = s.claim_at(ADDR, "", day * DAY_SECS + DAY_SECS / 2).unwrap();
        assert_eq!(second.balance_pradha, quota / 2);
        assert_eq!(second.last_claim_reward_pradha, quota / 2);
        assert_eq!(second.last_claim_payout_pradha, quota / 4);
        assert_eq!(s.claim_at(ADDR, "", day * DAY_SECS + DAY_SECS / 2), Err(ClaimError::NothingAccrued));
        let final_claim = s.claim_at(ADDR, "", day * DAY_SECS + DAY_SECS - 1).unwrap();
        assert!(final_claim.last_claim_reward_pradha >= quota.saturating_sub(1));
        assert_eq!(final_claim.balance_pradha, final_claim.last_claim_reward_pradha);
    }

    #[test]
    fn new_address_has_no_reward_before_elapsed_time() {
        let mut s = FaucetState::default();
        let day = START / DAY_SECS;
        assert_eq!(s.claim_at(ADDR, "", day * DAY_SECS).unwrap_err(), ClaimError::NothingAccrued);
        let first = s.claim_at(ADDR, "", day * DAY_SECS + 30 * 60).unwrap();
        assert_eq!(first.last_claim_payout_pradha, 20);
        let second = s.claim_at(ADDR, "", day * DAY_SECS + 60 * 60).unwrap();
        assert_eq!(second.last_claim_payout_pradha, 21);
        assert_eq!(second.balance_pradha, 41);
        assert!(s.claim_at(ADDR, "", day * DAY_SECS + DAY_SECS / 2).is_ok());
    }

    #[test]
    fn existing_claimed_balance_survives_day_rollover() {
        let mut s = FaucetState::default();
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
        let next = s.claim_at(ADDR, "", 11 * DAY_SECS + DAY_SECS / 2).unwrap();
        assert_eq!(next.balance_pradha, 1500);
        assert_eq!(next.last_claim_reward_pradha, 500);
        assert_eq!(next.daily_reward_pradha, 1000);
    }

    #[test]
    fn rejects_empty_address() {
        let mut s = FaucetState::default();
        assert_eq!(s.claim("", ""), Err(ClaimError::EmptyAddress));
        assert_eq!(s.claim("   ", ""), Err(ClaimError::EmptyAddress));
    }

    #[test]
    fn debit_deducts_balance() {
        let mut s = FaucetState::default();
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
        let mut s = FaucetState::default();
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
        let mut s = FaucetState::default();
        s.claim_day = FaucetState::current_day();
        // Cap harian dinamis: isi tepat sampai cap, klaim berikutnya harus ditolak.
        s.claimed_today = s.daily_cap_now();
        assert!(s.daily_cap_now() > 0, "cap dinamis harus > 0 di fase emisi");
        assert_eq!(s.claim(ADDR, ""), Err(ClaimError::DailyCapReached));
    }

    #[test]
    fn device_cap_limits_accounts() {
        let mut s = FaucetState::default();
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
        assert!(s.claim_at(sixth, "dev-2", START + 43_200).is_ok());
    }

    #[test]
    fn save_load_roundtrip() {
        let mut s = FaucetState::default();
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
        let mut s = FaucetState::default();
        assert_eq!(s.claim("not-an-address", "dev-x"), Err(ClaimError::InvalidAddress));
        assert!(s.claim_at("cen69:qzsgej828lu39wkvqfq8d2hwnul9ru79g3ja4ulj4y5cer25reym2unh2sjmw", "dev-x", START + 43_200).is_ok());
    }

    #[test]
    fn outstanding_cap_stops_issuance() {
        let mut s = FaucetState::default();
        let now = START + 43_200;
        let cap = crate::emission_policy::claim_bucket_pradha(0);
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
        assert_eq!(s.claim_at(ADDR, "", now), Err(ClaimError::ReservoirFull));
    }

    #[test]
    fn withdraw_cooldown_blocks_repeat() {
        let mut s = FaucetState::default();
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
        let mut s = FaucetState::default();
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

    /// Ambang tarik harus terjangkau satu klaim harian pada plafon penuh.
    /// Kalau ambang lebih besar daripada reward maksimum, saldo partisipasi
    /// menumpuk di ledger dan tidak pernah bisa dipindah on-chain.
    #[test]
    fn minimum_withdraw_is_reachable_from_one_daily_claim() {
        assert_eq!(FAUCET_MIN_WITHDRAW_PRADHA, 1_000, "ambang tarik harus 1.000 pradha");
        assert!(
            FAUCET_MIN_WITHDRAW_PRADHA <= FAUCET_MAX_REWARD_PRADHA,
            "ambang {} melebihi satu klaim penuh {}",
            FAUCET_MIN_WITHDRAW_PRADHA,
            FAUCET_MAX_REWARD_PRADHA
        );
    }

    /// Reward maximum 1000 pradha hanya berlaku saat peserta sedikit.
    /// Pada 2.025.000.000 peserta (seperempat penduduk bumi) yang memakai
    /// Android, reward harus menyusut proporsional — inilah yang membuat angka
    /// 1000 pradha sebuah *plafon*, bukan janji.
    #[test]
    fn reward_shrinks_as_participants_grow() {
        let mut s = FaucetState::default();
        s.claim_day = FaucetState::current_day();
        s.last_day_claimants = 0;
        assert_eq!(s.reward_pradha_today(), FAUCET_MAX_REWARD_PRADHA);

        // Cap inflow berkelanjutan ~1.078e8 pradha/hari, jadi plafon 1000 pradha
        // masih penuh sampai ~107.8 juta peserta/hari. Di atas ambang itu reward
        // harus menyusut proporsional agar total payout tidak melewati cap.
        s.last_day_claimants = 200_000_000;
        let r = s.reward_pradha_today();
        assert!(r < FAUCET_MAX_REWARD_PRADHA, "reward = {r}, harus menyusut");
        assert!(r >= FAUCET_MIN_REWARD_PRADHA, "reward = {r}");

        // 2,025 miliar peserta (seperempat penduduk bumi): reward = cap / N.
        // Dengan cap inflow bulan ini, ini jauh di bawah plafon tetapi (selama
        // cap >= N) masih > 1 pradha — artinya setiap peserta mendapat bagian
        // proporsional dan total payout tetap <= cap. "Lantai 1 pradha" hanya
        // tercapai ketika peserta melebihi sisa anggaran dalam satuan pradha.
        s.last_day_claimants = 2_025_000_000;
        let cap = s.daily_cap_now();
        let r = s.reward_pradha_today();
        let expected = (cap / 2_025_000_000).max(FAUCET_MIN_REWARD_PRADHA).min(FAUCET_MAX_REWARD_PRADHA);
        assert_eq!(r, expected, "reward harus proporsional: cap={cap}");
        assert!(r < FAUCET_MAX_REWARD_PRADHA);
        assert!(r >= FAUCET_MIN_REWARD_PRADHA);
    }

    #[test]
    fn daily_payout_never_exceeds_cap_under_stress() {
        let mut s = FaucetState::default();
        s.claim_day = START / DAY_SECS;
        let cap = crate::emission_policy::daily_cap_pradha(0);
        s.claimed_today = cap - 5500;
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
            if let Ok(entry) = s.claim_at(&address, "", START + 43_200) {
                paid += entry.last_claim_payout_pradha;
                successes += 1;
            }
            assert!(s.claimed_today <= cap);
        }
        assert!(successes > 0);
        assert!(paid > 0 && paid <= 5500);
        assert_eq!(s.claimed_today, initial + paid);
        assert_eq!(s.total_outstanding(), paid);
    }

    /// Setelah anggaran harian habis, klaim baru harus ditolak — bukan
    /// membayar dengan nilai sembarang.
    #[test]
    fn exhausted_budget_rejects_further_claims() {
        let mut s = FaucetState::default();
        s.claim_day = FaucetState::current_day();
        s.claimed_today = s.daily_cap_now();
        assert_eq!(s.reward_pradha_today(), 0);
        assert_eq!(s.claim(ADDR, ""), Err(ClaimError::DailyCapReached));
    }

    /// Reward yang dilaporkan API harus sama dengan yang benar-benar dibayar.
    #[test]
    fn reported_reward_matches_actual_payout() {
        let mut s = FaucetState::default();
        s.claim_day = START / DAY_SECS;
        s.last_day_claimants = 500_000;
        let now = START + 43_200;
        let before = s.claimable_at(ADDR, now);
        let entry = s.claim_at(ADDR, "", now).unwrap();
        assert_eq!(entry.last_claim_payout_pradha, before);
        assert_eq!(entry.last_claim_reward_pradha, before);
        assert_eq!(s.balance_response(ADDR, now)["rewardPradha"], 0);
    }
}
