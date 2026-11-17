//! Kebijakan reservoir faucet berdasarkan tabel subsidi mainnet.
//!
//! Floor awal 2.500.000 CEN menyusut tiap bulan sebanding dengan sisa emisi.
//! Cut menabung menuju floor dengan horizon 48 bulan dan deadband 1%.
//!
//! Bucket klaim/miner membagi target 60%/40%. Cap harian memakai `MAX_CUT`;
//! pemanggil tetap harus membatasi pengeluaran pada dana yang tersedia.
//!
//! Nilai pradha mengikuti pembulatan subsidi konsensus 10 BPS.

/// Detik per bulan (rata-rata 30,4375 hari), sama dengan konsensus.
pub const SECONDS_PER_MONTH: u64 = 2_629_800;

/// Blocks per second mainnet.
pub const BPS: u64 = 10;

/// Target awal reservoir: 2.500.000 CEN dalam pradha (1 CEN = 1e8 pradha).
pub const TARGET_RESERVOIR_PRADHA: u64 = 250_000_000_000_000;

/// Horizon perhitungan laju tabungan (bulan), bukan jaminan pengisian saldo.
pub const FILL_HORIZON_MONTHS: u64 = 48;

/// Beban maksimum yang boleh diambil dari reward miner (10%).
pub const MAX_CUT_FRACTION: f64 = 0.10;

/// Cut minimum saat masih ada defisit (0,5%).
pub const MIN_CUT_FRACTION: f64 = 0.005;

/// Defisit dalam 1% floor dianggap penuh agar cut stabil terhadap pembulatan.
pub const DEADBAND_FRACTION: f64 = 0.01;

/// Porsi target reservoir untuk subsidi miner Android (40%); sisanya klaim.
pub const MINER_SUBSIDY_SHARE: f64 = 0.40;

/// Pembagi pelaporan subsidi per perangkat, bukan batas jumlah miner aktif.
pub const TARGET_MINER_COUNT: u32 = 10;

/// Hari per bulan sebagai f64, untuk konversi laju harian.
const DAYS_PER_MONTH: f64 = 30.4375;

/// Timestamp genesis mainnet (7 Nov 2026 20:00:00 UTC).
pub const GENESIS_TIMESTAMP_SECS: u64 = 1_794_081_600;

/// Tabel subsidi per bulan (1 BPS), dibaca dari `scripts/data/subsidy_table.rs`
/// yang sama dengan konsensus. Nilai = pradha per detik (per blok @1BPS).
const SUBSIDY_BY_MONTH_TABLE: [u64; 817] = [
    20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466,
    20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466,
    20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466,
    20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 20795466, 10397733, 10397733, 10397733, 10397733,
    10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733,
    10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733,
    10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733, 10397733,
    10397733, 10397733, 10397733, 10397733, 10397733, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867,
    5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867,
    5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867,
    5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 5198867, 2599433, 2599433, 2599433,
    2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433,
    2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433,
    2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433, 2599433,
    2599433, 2599433, 2599433, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717,
    1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717,
    1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717,
    1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 1299717, 649858, 649858, 649858, 649858, 649858, 649858,
    649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858,
    649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858,
    649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 649858, 324929, 324929, 324929, 324929, 324929, 324929,
    324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929,
    324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929,
    324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 324929, 162465, 162465, 162465, 162465, 162465, 162465,
    162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465,
    162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465,
    162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 162465, 81232, 81232, 81232, 81232, 81232, 81232, 81232,
    81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232,
    81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232, 81232,
    81232, 81232, 81232, 81232, 81232, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616,
    40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616,
    40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 40616, 20308,
    20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308,
    20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308,
    20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 20308, 10154, 10154, 10154, 10154, 10154, 10154, 10154,
    10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154,
    10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154, 10154,
    10154, 10154, 10154, 10154, 10154, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077,
    5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077,
    5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 5077, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539,
    2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539,
    2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 2539, 1269, 1269, 1269, 1269,
    1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269,
    1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269, 1269,
    1269, 1269, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635,
    635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 635, 317,
    317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317,
    317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 317, 0,
];

/// Kembalikan jumlah elemen tabel (untuk sanity check di tes).
pub const fn table_len() -> usize {
    SUBSIDY_BY_MONTH_TABLE.len()
}

/// Bulan sejak genesis untuk `now_secs` (detik Unix).
pub fn month_since_genesis(now_secs: u64) -> u64 {
    now_secs.saturating_sub(GENESIS_TIMESTAMP_SECS) / SECONDS_PER_MONTH
}

/// Subsidi per bulan (pradha) pada bulan ke-`month`, sesuai 10 BPS.
/// Konsensus memakai `tabel[i].div_ceil(BPS)` per blok lalu × BPS × detik
/// bulan — pembulatan ke atas agar anggaran tidak pernah diremehkan.
pub fn subsidy_pradha_month(month: u64) -> u64 {
    let m = month.min(SUBSIDY_BY_MONTH_TABLE.len() as u64 - 1);
    let v = SUBSIDY_BY_MONTH_TABLE[m as usize];
    let per_block = v.div_ceil(BPS);
    per_block.saturating_mul(BPS).saturating_mul(SECONDS_PER_MONTH)
}

/// Sisa emisi (pradha) sejak bulan `month`: jumlah seluruh subsidi bulanan
/// dari `month` sampai akhir tabel.
pub fn remaining_emission_pradha(month: u64) -> u64 {
    let start = (month as usize).min(SUBSIDY_BY_MONTH_TABLE.len() - 1);
    SUBSIDY_BY_MONTH_TABLE[start..].iter().fold(0u64, |acc, &v| {
        let per_block = v.div_ceil(BPS);
        acc.saturating_add(per_block.saturating_mul(BPS).saturating_mul(SECONDS_PER_MONTH))
    })
}

/// Target awal dikali rasio sisa emisi terhadap emisi awal, dibulatkan ke bawah.
/// Perkalian memakai u128 agar presisi pradha terjaga tanpa overflow.
pub fn floor_pradha(month: u64) -> u64 {
    let remaining = remaining_emission_pradha(month) as u128;
    let initial = remaining_emission_pradha(0) as u128;
    ((TARGET_RESERVOIR_PRADHA as u128 * remaining) / initial) as u64
}

/// Emisi kumulatif dari bulan `from` sampai `to` (eksklusif), dalam pradha.
pub fn emission_to(from: u64, to: u64) -> u64 {
    let len = SUBSIDY_BY_MONTH_TABLE.len() as u64;
    let start = from.min(len) as usize;
    let end = to.min(len) as usize;
    if start >= end {
        return 0;
    }
    SUBSIDY_BY_MONTH_TABLE[start..end].iter().fold(0u64, |acc, &v| {
        let per_block = v.div_ceil(BPS);
        acc.saturating_add(per_block.saturating_mul(BPS).saturating_mul(SECONDS_PER_MONTH))
    })
}

/// Cut menabung `floor × emisi_bulan / emisi_horizon`, dibatasi defisit dan
/// rentang cut. Saldo dalam deadband floor mematikan pemotongan.
pub fn cut_fraction(month: u64, balance_pradha: u64) -> f64 {
    let floor = floor_pradha(month);
    let deficit = floor.saturating_sub(balance_pradha);

    // Saldo sudah di dalam zona mati floor → tidak perlu memotong sama sekali.
    if deficit == 0 || (deficit as f64) <= (floor as f64) * DEADBAND_FRACTION {
        return 0.0;
    }

    let horizon_emission = emission_to(month, month.saturating_add(FILL_HORIZON_MONTHS));
    if horizon_emission == 0 {
        // Emisi sudah berakhir: tidak ada lagi yang bisa disisihkan.
        return 0.0;
    }

    let month_emission = subsidy_pradha_month(month);
    if month_emission == 0 {
        return 0.0;
    }

    let target_inflow = (floor as f64) * (month_emission as f64) / (horizon_emission as f64);
    let inflow = target_inflow.min(deficit as f64);
    if inflow <= 0.0 {
        return 0.0;
    }
    let cut = inflow / (month_emission as f64);
    cut.clamp(MIN_CUT_FRACTION, MAX_CUT_FRACTION)
}

/// Porsi reservoir yang dialokasikan untuk klaim faucet (60%).
///
/// Komplemen dari [`MINER_SUBSIDY_SHARE`]; dinyatakan sebagai fungsi agar tidak
/// ada dua konstanta yang bisa tidak berjumlah 1,0.
pub fn claim_share() -> f64 {
    1.0 - MINER_SUBSIDY_SHARE
}

/// Porsi target untuk klaim faucet; saldo aktual tetap diperiksa pemanggil.
pub fn claim_bucket_pradha(month: u64) -> u64 {
    (floor_pradha(month) as f64 * claim_share()) as u64
}

/// Bucket subsidi miner Android pada bulan `month`, dalam pradha.
pub fn miner_bucket_pradha(month: u64) -> u64 {
    (floor_pradha(month) as f64 * MINER_SUBSIDY_SHARE) as u64
}

/// Plafon subsidi harian semua miner pada `MAX_CUT`, tanpa amortisasi saldo.
/// Nilai ini bukan dana tersedia; pemanggil memeriksa saldo dan inflow aktual.
pub fn miner_subsidy_daily_pradha(month: u64) -> u64 {
    let monthly_inflow = (subsidy_pradha_month(month) as f64 * MAX_CUT_FRACTION * MINER_SUBSIDY_SHARE) as u64;
    (monthly_inflow as f64 / DAYS_PER_MONTH) as u64
}

/// Subsidi harian untuk satu miner bila ada `miner_count` miner aktif.
///
/// Dibagi rata dan dibatasi agar tidak pernah melebihi anggaran agregat. Kalau
/// `miner_count` = 0, hasilnya 0 (tidak ada yang dibayar).
pub fn miner_subsidy_per_miner_pradha(month: u64, miner_count: u32) -> u64 {
    if miner_count == 0 {
        return 0;
    }
    miner_subsidy_daily_pradha(month) / miner_count as u64
}

/// Plafon klaim harian pada `MAX_CUT`, memakai porsi klaim tanpa amortisasi saldo.
/// Nilai ini bukan dana tersedia; pemanggil memeriksa saldo dan inflow aktual.
pub fn daily_cap_pradha(month: u64) -> u64 {
    let monthly_inflow = (subsidy_pradha_month(month) as f64 * MAX_CUT_FRACTION * claim_share()) as u64;
    (monthly_inflow as f64 / DAYS_PER_MONTH) as u64
}

/// Ringkasan kebijakan untuk satu momen waktu (dipakai endpoint status).
pub struct EmissionPolicySnapshot {
    pub month: u64,
    pub remaining_emission_pradha: u64,
    pub cut_fraction: f64,
    pub floor_pradha: u64,
    pub daily_cap_pradha: u64,
}

/// Hitung snapshot kebijakan untuk `now_secs` dan saldo reservoir saat ini.
pub fn snapshot(now_secs: u64, balance_pradha: u64) -> EmissionPolicySnapshot {
    let month = month_since_genesis(now_secs);
    EmissionPolicySnapshot {
        month,
        remaining_emission_pradha: remaining_emission_pradha(month),
        cut_fraction: cut_fraction(month, balance_pradha),
        floor_pradha: floor_pradha(month),
        daily_cap_pradha: daily_cap_pradha(month),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_length_is_817() {
        assert_eq!(table_len(), 817);
    }

    #[test]
    fn month_zero_is_genesis() {
        assert_eq!(month_since_genesis(GENESIS_TIMESTAMP_SECS), 0);
        // Beberapa menit setelah genesis masih bulan 0.
        assert_eq!(month_since_genesis(GENESIS_TIMESTAMP_SECS + 3600), 0);
    }

    #[test]
    fn empty_reservoir_cuts_at_savings_rate() {
        // Saldo 0 di bulan 0: reservoir menabung tepat `floor / E_48` dari
        // setiap bulan. Nilainya di bawah plafon 10% karena target hanya 4,8%
        // dari total emisi dan horizon 48 bulan masih menyisakan emisi besar.
        let cut = cut_fraction(0, 0);
        let expected = floor_pradha(0) as f64 / emission_to(0, 48) as f64;
        assert!((cut - expected).abs() < 1e-9, "cut = {cut}, expected = {expected}");
        assert!(cut > MIN_CUT_FRACTION, "cut = {cut}");
        assert!(cut <= MAX_CUT_FRACTION, "cut = {cut}");
    }

    #[test]
    fn full_reservoir_stops_cutting() {
        let floor = floor_pradha(0);
        assert_eq!(cut_fraction(0, floor), 0.0, "saldo = floor → cut 0");
        assert_eq!(cut_fraction(0, floor + 1), 0.0, "saldo di atas floor → cut 0");
    }

    #[test]
    fn deadband_absorbs_near_full_balance() {
        let floor = floor_pradha(0);
        // Tepat di batas deadband: dianggap penuh.
        let near = floor - (floor as f64 * DEADBAND_FRACTION) as u64;
        assert_eq!(cut_fraction(0, near), 0.0);
        // Sedikit di luar deadband: mulai memotong.
        let outside = floor - (floor as f64 * DEADBAND_FRACTION) as u64 - 1;
        assert!(cut_fraction(0, outside) > 0.0);
    }

    #[test]
    fn cut_rises_when_balance_is_drained() {
        let floor = floor_pradha(0);
        let half = floor / 2;
        let c_half = cut_fraction(0, half);
        let c_empty = cut_fraction(0, 0);
        assert!(c_empty >= c_half, "saldo lebih kecil harus memotong lebih banyak");
        assert!(c_half > 0.0);
    }

    #[test]
    fn simulation_reaches_dynamic_target_without_spending() {
        let mut balance = 0u64;
        let mut reached = None;
        for month in 0..table_len() as u64 {
            let cut = cut_fraction(month, balance);
            let inflow = (subsidy_pradha_month(month) as f64 * cut) as u64;
            balance = balance.saturating_add(inflow);
            if reached.is_some() {
                assert_eq!(cut, 0.0, "cut menyala lagi di bulan {month}");
            } else if balance >= floor_pradha(month) {
                reached = Some(month + 1);
            }
        }
        let elapsed_months = reached.expect("target dinamis harus tercapai tanpa pengeluaran");
        assert!(
            elapsed_months <= FILL_HORIZON_MONTHS,
            "target tercapai setelah {elapsed_months} bulan, melewati horizon {FILL_HORIZON_MONTHS}"
        );
    }

    /// Setelah target tercapai dan tidak ada pengurasan, cut harus mati:
    /// reward kembali penuh ke miner.
    #[test]
    fn cut_stays_off_once_target_reached() {
        let month = FILL_HORIZON_MONTHS;
        assert_eq!(cut_fraction(month, floor_pradha(month)), 0.0, "reservoir penuh → reward penuh ke miner");
    }

    /// Pengurasan di tengah jalan harus menyalakan cut lagi (self-correcting).
    #[test]
    fn drain_after_fill_turns_cut_back_on() {
        let month = FILL_HORIZON_MONTHS + 12;
        let floor = floor_pradha(month);
        assert_eq!(cut_fraction(month, floor), 0.0);
        let drained = floor / 2;
        assert!(cut_fraction(month, drained) > 0.0, "saldo terkuras → cut hidup lagi");
    }

    #[test]
    fn floor_never_exceeds_target() {
        for m in 0..=table_len() as u64 {
            assert!(floor_pradha(m) <= TARGET_RESERVOIR_PRADHA);
        }
    }

    #[test]
    fn floor_declines_from_month_one() {
        assert!(floor_pradha(1) < floor_pradha(0));
    }

    #[test]
    fn floor_starts_at_initial_target() {
        assert_eq!(floor_pradha(0), TARGET_RESERVOIR_PRADHA);
    }

    #[test]
    fn floor_declines_each_month_while_emission_remains() {
        for month in 0..table_len() as u64 - 1 {
            assert!(floor_pradha(month + 1) < floor_pradha(month), "floor tidak turun setelah bulan {month}");
        }
    }

    #[test]
    fn floor_is_zero_after_emission_ends() {
        let end = table_len() as u64 - 1;
        for month in [end, end + 1, u64::MAX] {
            assert_eq!(floor_pradha(month), 0, "floor bulan {month}");
        }
    }

    #[test]
    fn floor_never_exceeds_remaining_emission() {
        for month in 0..=table_len() as u64 {
            assert!(floor_pradha(month) <= remaining_emission_pradha(month), "floor melebihi sisa emisi bulan {month}");
        }
    }

    #[test]
    fn floor_tracks_remaining_emission_rounded_down() {
        let initial = remaining_emission_pradha(0) as u128;
        assert!(initial > TARGET_RESERVOIR_PRADHA as u128);
        for month in 0..=table_len() as u64 {
            let numerator = (TARGET_RESERVOIR_PRADHA as u128)
                .checked_mul(remaining_emission_pradha(month) as u128)
                .expect("produksi floor harus muat dalam u128");
            let floor = floor_pradha(month) as u128;
            let lower = floor.checked_mul(initial).expect("batas bawah harus muat dalam u128");
            let upper = lower.checked_add(initial).expect("batas atas harus muat dalam u128");
            assert!(lower <= numerator && numerator < upper, "pembulatan floor bulan {month}");
        }
    }

    #[test]
    fn floor_intermediate_product_requires_u128() {
        let product = (TARGET_RESERVOIR_PRADHA as u128)
            .checked_mul(remaining_emission_pradha(0) as u128)
            .expect("produksi floor awal harus muat dalam u128");
        assert!(product > u64::MAX as u128);
        assert_eq!(floor_pradha(0), TARGET_RESERVOIR_PRADHA);
    }

    #[test]
    fn cut_never_exceeds_max_even_when_desperate() {
        // Bulan-bulan terakhir dengan saldo 0: laju boleh naik, tapi tidak
        // melewati MAX_CUT — pagar kepercayaan miner.
        let last = table_len() as u64 - 2;
        let cut = cut_fraction(last, 0);
        assert!(cut <= MAX_CUT_FRACTION, "cut = {cut}");
    }

    #[test]
    fn zero_cut_when_emission_ended() {
        let last = table_len() as u64 - 1;
        assert_eq!(cut_fraction(last, 0), 0.0, "emisi habis → tidak ada yang bisa disisihkan");
        assert_eq!(daily_cap_pradha(last), 0);
    }

    #[test]
    fn daily_cap_positive_while_emission_runs() {
        for m in [0u64, 48, 120, 300, 600] {
            assert!(daily_cap_pradha(m) > 0, "cap di bulan {m} harus > 0");
        }
    }

    #[test]
    fn daily_cap_covers_one_full_max_claim() {
        // Cap harian harus mengizinkan setidaknya satu klaim pada plafon penuh
        // selama fase emisi — kalau tidak, reward maksimum 1000 pradha tidak
        // pernah bisa dibayar.
        let max_reward = 1_000u64;
        for m in [0u64, 24, 48, 120] {
            assert!(daily_cap_pradha(m) >= max_reward, "cap bulan {m} = {} < 1 klaim penuh", daily_cap_pradha(m));
        }
    }

    #[test]
    fn emission_to_sums_months_correctly() {
        assert_eq!(emission_to(5, 5), 0, "rentang kosong = 0");
        assert_eq!(emission_to(7, 3), 0, "rentang terbalik = 0");
        let one = emission_to(0, 1);
        assert_eq!(one, subsidy_pradha_month(0));
        let span = emission_to(0, 3);
        assert_eq!(span, subsidy_pradha_month(0) + subsidy_pradha_month(1) + subsidy_pradha_month(2));
        // Melewati akhir tabel tetap aman (clamp), tidak panik.
        assert_eq!(emission_to(0, u64::MAX), remaining_emission_pradha(0));
    }

    #[test]
    fn snapshot_reports_consistent_policy() {
        let s = snapshot(GENESIS_TIMESTAMP_SECS, 0);
        assert_eq!(s.month, 0);
        assert_eq!(s.floor_pradha, TARGET_RESERVOIR_PRADHA);
        assert_eq!(s.cut_fraction, cut_fraction(0, 0));
        assert!(s.cut_fraction <= MAX_CUT_FRACTION);
        assert_eq!(s.daily_cap_pradha, daily_cap_pradha(0));
    }
}
