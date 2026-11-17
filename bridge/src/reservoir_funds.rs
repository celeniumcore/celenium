//! Cash backing prevents new claims or surplus sweeps from consuming existing liabilities.

pub const SECONDS_PER_DAY: u64 = 86_400;
pub const FUNDING_MAX_AGE_SECS: u64 = 30;
pub const RESERVE_HORIZON_DAYS: u64 = 365;
// Keep daily payout fees funded alongside claim liabilities.
pub const OPERATIONAL_BUFFER_PRADHA: u64 = 250_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct FundingSnapshot {
    pub mature_pradha: u64,
    pub observed_at: u64,
    pub month: u64,
}

impl FundingSnapshot {
    pub fn is_fresh(&self, now: u64) -> bool {
        matches!(now.checked_sub(self.observed_at), Some(age) if age <= FUNDING_MAX_AGE_SECS)
    }
}

pub const FAUCET_RESERVE_LIMIT_PRADHA: u64 = 50_000_000_000_000;

pub fn reserve_target_pradha(floor: u64) -> u64 {
    floor.min(FAUCET_RESERVE_LIMIT_PRADHA)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FaucetFundingSnapshot {
    pub operational: FundingSnapshot,
    pub reserve: Option<FundingSnapshot>,
    pub reserve_configured: bool,
}

impl FaucetFundingSnapshot {
    pub fn combined(&self, now: u64, floor: u64, outstanding: u64) -> Option<FundingSnapshot> {
        if !self.operational.is_fresh(now) {
            return None;
        }
        if !self.reserve_configured {
            return self.reserve.is_none().then_some(self.operational);
        }
        let reserve = self.reserve.filter(|f| f.is_fresh(now) && f.month == self.operational.month)?;
        // A shrinking target cannot withdraw backing already owed to claimants.
        let backing = outstanding.saturating_add(OPERATIONAL_BUFFER_PRADHA).saturating_sub(self.operational.mature_pradha);
        let eligible = reserve.mature_pradha.min(reserve_target_pradha(floor).max(backing));
        Some(FundingSnapshot {
            mature_pradha: self.operational.mature_pradha.checked_add(eligible)?,
            observed_at: self.operational.observed_at.min(reserve.observed_at),
            month: self.operational.month,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayoutSource {
    Operational,
    Reserve,
}

pub fn select_withdrawal_source(
    operational_fee: Option<u64>,
    reserve_fee: Option<u64>,
    amount: u64,
    outstanding: u64,
    operational_mature: u64,
    reserve_eligible: u64,
    combined: u64,
) -> Option<PayoutSource> {
    for (source, fee, mature) in
        [(PayoutSource::Operational, operational_fee, operational_mature), (PayoutSource::Reserve, reserve_fee, reserve_eligible)]
    {
        let Some(cost) = fee.and_then(|fee| amount.checked_add(fee)) else { continue };
        if amount > 0
            && amount <= outstanding
            && cost <= mature
            && combined.checked_sub(cost).is_some_and(|remaining| remaining >= outstanding - amount)
        {
            return Some(source);
        }
    }
    None
}

pub fn available_for_claims(mature: u64, outstanding: u64) -> u64 {
    mature.saturating_sub(outstanding).saturating_sub(OPERATIONAL_BUFFER_PRADHA)
}

/// Funded schedules keep their quota; reserves sustain claims after emission stops.
pub fn cash_daily_cap(mature: u64, outstanding: u64, scheduled_cap: u64) -> u64 {
    let free = available_for_claims(mature, outstanding);
    free.min(scheduled_cap.max(free / RESERVE_HORIZON_DAYS).max(if free >= 1_000 { 1_000 } else { 0 }))
}

pub fn protected_balance(floor: u64, outstanding: u64) -> u64 {
    protected_balance_with_service_reserve(floor, outstanding, 0)
}

pub fn sweep_amount(mature: u64, floor: u64, outstanding: u64, fee: u64) -> u64 {
    sweep_amount_with_service_reserve(mature, floor, outstanding, 0, fee)
}

/// Include payouts and their fees in daily_spend; this reserve target never credits funds.
pub fn service_reserve(daily_spend: u64) -> u64 {
    daily_spend.max(1_000).saturating_mul(RESERVE_HORIZON_DAYS)
}

pub fn protected_balance_with_service_reserve(floor: u64, outstanding: u64, service_reserve: u64) -> u64 {
    floor.max(outstanding.saturating_add(OPERATIONAL_BUFFER_PRADHA).saturating_add(service_reserve))
}

pub fn sweep_amount_with_service_reserve(mature: u64, floor: u64, outstanding: u64, service_reserve: u64, fee: u64) -> u64 {
    mature.saturating_sub(protected_balance_with_service_reserve(floor, outstanding, service_reserve)).saturating_sub(fee)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_operational_wallet_can_use_a_funded_reserve() {
        let sources = FaucetFundingSnapshot {
            operational: FundingSnapshot { mature_pradha: 0, observed_at: 100, month: 0 },
            reserve: Some(FundingSnapshot { mature_pradha: 50_000_000_000_000, observed_at: 100, month: 0 }),
            reserve_configured: true,
        };
        let combined = sources.combined(100, 250_000_000_000_000, 0).unwrap();
        assert_eq!(combined.mature_pradha, 50_000_000_000_000);
        assert!(available_for_claims(combined.mature_pradha, 0) >= 1000);
        assert_eq!(sources.operational.mature_pradha, 0);
    }

    #[test]
    fn reserve_target_never_creates_cash() {
        let sources = FaucetFundingSnapshot {
            operational: FundingSnapshot { observed_at: 100, ..Default::default() },
            reserve: Some(FundingSnapshot { observed_at: 100, ..Default::default() }),
            reserve_configured: true,
        };
        assert_eq!(sources.combined(100, u64::MAX, 0).unwrap().mature_pradha, 0);
        assert_eq!(reserve_target_pradha(u64::MAX), 50_000_000_000_000);
    }

    #[test]
    fn missing_or_stale_either_source_fails_closed() {
        let mut sources = FaucetFundingSnapshot {
            operational: FundingSnapshot { mature_pradha: u64::MAX, observed_at: 100, month: 0 },
            reserve: None,
            reserve_configured: true,
        };
        assert!(sources.combined(100, 1, 0).is_none());
        sources.reserve = Some(FundingSnapshot { observed_at: 69, ..Default::default() });
        assert!(sources.combined(100, 1, 0).is_none());
        sources.reserve = Some(FundingSnapshot { observed_at: 100, ..Default::default() });
        sources.operational.observed_at = 69;
        assert!(sources.combined(100, 1, 0).is_none());
        sources.operational.observed_at = 100;
        sources.reserve.as_mut().unwrap().month = 1;
        assert!(sources.combined(100, 1, 0).is_none());
    }

    #[test]
    fn reduced_target_preserves_only_actual_legacy_backing() {
        let outstanding = 60_000_000_000_000;
        let needed = outstanding + OPERATIONAL_BUFFER_PRADHA;
        let sources = FaucetFundingSnapshot {
            operational: FundingSnapshot { observed_at: 100, ..Default::default() },
            reserve: Some(FundingSnapshot { mature_pradha: needed + 1000, observed_at: 100, month: 0 }),
            reserve_configured: true,
        };
        let combined = sources.combined(100, 0, outstanding).unwrap();
        assert_eq!(combined.mature_pradha, needed);
        assert_eq!(available_for_claims(combined.mature_pradha, outstanding), 0);
        let mut short = sources;
        short.reserve.as_mut().unwrap().mature_pradha = 1000;
        assert_eq!(short.combined(100, 0, outstanding).unwrap().mature_pradha, 1000);
    }

    #[test]
    fn disabled_reserve_preserves_operational_funding() {
        let sources = FaucetFundingSnapshot {
            operational: FundingSnapshot { mature_pradha: 350_000_000, observed_at: 100, month: 3 },
            reserve: None,
            reserve_configured: false,
        };
        let combined = sources.combined(100, 0, 0).unwrap();
        assert_eq!((combined.mature_pradha, combined.month, combined.observed_at), (350_000_000, 3, 100));
    }

    #[test]
    fn fallback_selector_requires_one_source_and_combined_solvency() {
        assert_eq!(select_withdrawal_source(None, Some(275), 1000, 1000, 0, 5000, 5000), Some(PayoutSource::Reserve));
        assert_eq!(select_withdrawal_source(Some(275), Some(275), 1000, 1000, 5000, 5000, 10000), Some(PayoutSource::Operational));
        assert_eq!(select_withdrawal_source(None, Some(275), 1000, 1000, 1100, 5000, 6100), Some(PayoutSource::Reserve));
        assert_eq!(select_withdrawal_source(None, Some(275), 1000, 5000, 0, 5000, 5000), None);
        assert_eq!(select_withdrawal_source(Some(275), Some(275), 1000, 1000, 1000, 1000, 2000), None);
        assert_eq!(select_withdrawal_source(None, Some(u64::MAX), 1000, 1000, 0, u64::MAX, u64::MAX), None);
    }

    #[test]
    fn end_emission_reserve_funds_a_fixed_claim_on_each_of_365_days() {
        let mut free = service_reserve(1000);
        for day in 0..365 {
            assert!(cash_daily_cap(OPERATIONAL_BUFFER_PRADHA + free, 0, 0) >= 1000, "day {day} still has {free} pradha");
            free -= 1000;
        }
        assert_eq!(cash_daily_cap(OPERATIONAL_BUFFER_PRADHA + free, 0, 0), 0);
    }

    #[test]
    fn service_reserve_sweep_never_invents_spendable_cash() {
        let values = [0, 1, 365_000, 250_000_000, 350_365_000, u64::MAX - 1, u64::MAX];
        for mature in values {
            for floor in values {
                for outstanding in values {
                    for reserve in values {
                        let commitments = outstanding as u128 + 250_000_000 + reserve as u128;
                        let protected = (floor as u128).max(commitments.min(u64::MAX as u128));
                        assert_eq!(protected_balance_with_service_reserve(floor, outstanding, reserve), protected as u64);
                        for fee in values {
                            let cost = protected + fee as u128;
                            let expected = if mature as u128 > cost { (mature as u128 - cost) as u64 } else { 0 };
                            let swept = sweep_amount_with_service_reserve(mature, floor, outstanding, reserve, fee);
                            assert_eq!(swept, expected);
                            if swept > 0 {
                                assert!(swept as u128 + fee as u128 + protected <= mature as u128);
                            }
                            if reserve == 0 {
                                assert_eq!(sweep_amount(mature, floor, outstanding, fee), swept);
                                assert_eq!(protected_balance(floor, outstanding), protected as u64);
                            }
                        }
                    }
                }
            }
        }
        let mature = 350_000_100;
        let outstanding = 100_000_000;
        let target = service_reserve(0);
        assert!(target > available_for_claims(mature, outstanding));
        assert_eq!(sweep_amount_with_service_reserve(mature, 0, outstanding, target, 275), 0);
        assert_eq!(available_for_claims(mature, outstanding), 100);
        assert_eq!(cash_daily_cap(mature, outstanding, 0), 0);
    }

    #[test]
    fn snapshots_expire_after_thirty_seconds_without_accepting_clock_regressions() {
        assert_eq!(FUNDING_MAX_AGE_SECS, 30);
        assert_eq!(SECONDS_PER_DAY, 86_400);
        fn require_snapshot_traits<T: Clone + Copy + std::fmt::Debug + Default>() {}
        require_snapshot_traits::<FundingSnapshot>();
        let default = FundingSnapshot::default();
        assert_eq!((default.mature_pradha, default.observed_at, default.month), (0, 0, 0));
        for (observed_at, now, expected) in [
            (100, 100, true),
            (100, 129, true),
            (100, 130, true),
            (100, 131, false),
            (100, 99, false),
            (1, 0, false),
            (0, 0, true),
            (0, u64::MAX, false),
            (u64::MAX, u64::MAX, true),
            (u64::MAX, u64::MAX - 1, false),
            (u64::MAX - 30, u64::MAX, true),
            (u64::MAX - 31, u64::MAX, false),
        ] {
            let snapshot = FundingSnapshot { mature_pradha: 42, observed_at, month: 264 };
            assert_eq!(snapshot.is_fresh(now), expected, "observed_at={observed_at}, now={now}");
            assert_eq!((snapshot.mature_pradha, snapshot.month), (42, 264));
        }
    }

    #[test]
    fn end_emission_sweep_retains_a_year_of_minimum_claims() {
        let mature = 400_000_000;
        let outstanding = 100_000_000;
        let fee = 275;
        let reserve = service_reserve(0);
        let swept = sweep_amount_with_service_reserve(mature, 0, outstanding, reserve, fee);
        assert_eq!(swept, 400_000_000 - 350_365_000 - 275);
        let retained = mature - swept - fee;
        assert_eq!(retained, 350_365_000);
        assert_eq!(available_for_claims(retained, outstanding), 365_000);
        assert_eq!(cash_daily_cap(retained, outstanding, 0), 1_000);
    }

    #[test]
    fn protection_adds_service_reserve_beyond_liabilities_and_buffer() {
        for (floor, outstanding, reserve, expected) in [
            (0, 100_000_000, 365_000, 350_365_000),
            (0, 0, 365_000, 250_365_000),
            (350_100_000, 100_000_000, 365_000, 350_365_000),
            (500_000_000, 100_000_000, 365_000, 500_000_000),
            (0, u64::MAX, 365_000, u64::MAX),
            (0, 0, u64::MAX, u64::MAX),
            (0, u64::MAX - 250_000_000, 1, u64::MAX),
            (u64::MAX, 0, 365_000, u64::MAX),
            (0, 100_000_000, 0, 350_000_000),
        ] {
            assert_eq!(
                protected_balance_with_service_reserve(floor, outstanding, reserve),
                expected,
                "floor={floor}, outstanding={outstanding}, reserve={reserve}"
            );
        }
    }

    #[test]
    fn service_reserve_covers_a_year_of_at_least_one_daily_claim() {
        for (daily_spend, expected) in [
            (0, 365_000),
            (999, 365_000),
            (1_000, 365_000),
            (1_275, 465_375),
            (10_000, 3_650_000),
            (u64::MAX / 365, (u64::MAX / 365) * 365),
            (u64::MAX / 365 + 1, u64::MAX),
            (u64::MAX, u64::MAX),
        ] {
            assert_eq!(service_reserve(daily_spend), expected, "daily_spend={daily_spend}");
        }
    }

    #[test]
    fn sweep_spends_only_surplus_after_protection_and_fee() {
        for (mature, floor, outstanding, fee, expected) in [
            (0, 0, 0, 0, 0),
            (300_000_000, 0, 100_000_000, 275, 0),
            (400_000_000, 0, 100_000_000, 275, 49_999_725),
            (600_000_000, 500_000_000, 100_000_000, 275, 99_999_725),
            (350_000_000, 0, 100_000_000, 275, 0),
            (350_000_275, 0, 100_000_000, 275, 0),
            (350_000_276, 0, 100_000_000, 275, 1),
            (400_000_000, 0, 100_000_000, u64::MAX, 0),
            (u64::MAX, 0, u64::MAX, 0, 0),
            (u64::MAX, u64::MAX, 0, 0, 0),
            (u64::MAX, 0, 0, 0, u64::MAX - 250_000_000),
            (u64::MAX, 0, 0, u64::MAX, 0),
        ] {
            assert_eq!(
                sweep_amount(mature, floor, outstanding, fee),
                expected,
                "mature={mature}, floor={floor}, outstanding={outstanding}, fee={fee}"
            );
            if expected > 0 {
                assert!(mature - expected - fee >= protected_balance(floor, outstanding));
            }
        }
    }

    #[test]
    fn protection_keeps_the_greater_of_floor_or_liability_backing() {
        for (floor, outstanding, expected) in [
            (0, 0, 250_000_000),
            (0, 100_000_000, 350_000_000),
            (500_000_000, 100_000_000, 500_000_000),
            (300_000_000, 100_000_000, 350_000_000),
            (u64::MAX, 0, u64::MAX),
            (0, u64::MAX, u64::MAX),
            (0, u64::MAX - 100, u64::MAX),
            (0, u64::MAX - 250_000_000, u64::MAX),
        ] {
            assert_eq!(protected_balance(floor, outstanding), expected, "floor={floor}, outstanding={outstanding}");
        }
    }

    #[test]
    fn reserves_fund_claims_after_emission_ends() {
        for (mature, outstanding, scheduled_cap, expected) in [
            (0, 0, 0, 0),
            (250_000_364, 0, 0, 0),
            (250_000_365, 0, 0, 1),
            (250_365_000, 0, 0, 1_000),
            (350_365_000, 100_000_000, 0, 1_000),
            (350_365_000, 100_000_000, 999, 1_000),
            (350_365_000, 100_000_000, 1_001, 1_001),
            (u64::MAX, 0, 0, (u64::MAX - 250_000_000) / 365),
        ] {
            assert_eq!(
                cash_daily_cap(mature, outstanding, scheduled_cap),
                expected,
                "mature={mature}, outstanding={outstanding}, scheduled_cap={scheduled_cap}"
            );
        }
    }

    #[test]
    fn scheduled_quota_is_limited_by_backing() {
        for (mature, outstanding, scheduled_cap, expected) in [
            (0, 0, u64::MAX, 0),
            (250_000_000, 0, u64::MAX, 0),
            (300_000_000, 400_000_000, 1_000, 0),
            (250_005_000, 0, 1_000, 1_000),
            (250_005_000, 0, 10_000, 5_000),
            (350_005_000, 100_000_000, 1_000, 1_000),
            (u64::MAX, u64::MAX, u64::MAX, 0),
            (u64::MAX, 0, u64::MAX, u64::MAX - 250_000_000),
        ] {
            assert_eq!(
                cash_daily_cap(mature, outstanding, scheduled_cap),
                expected,
                "mature={mature}, outstanding={outstanding}, scheduled_cap={scheduled_cap}"
            );
        }
    }

    #[test]
    fn claims_cannot_spend_outstanding_liabilities() {
        for (mature, outstanding, expected) in [
            (300_000_000, 40_000_000, 10_000_000),
            (300_000_000, 50_000_000, 0),
            (300_000_000, 50_000_001, 0),
            (300_000_000, 400_000_000, 0),
            (0, u64::MAX, 0),
            (u64::MAX, u64::MAX, 0),
            (u64::MAX, u64::MAX - 100, 0),
            (u64::MAX, 1, u64::MAX - 250_000_001),
        ] {
            assert_eq!(available_for_claims(mature, outstanding), expected, "mature={mature}, outstanding={outstanding}");
        }
    }

    #[test]
    fn claims_retain_the_operational_buffer() {
        for (mature, expected) in [
            (0, 0),
            (249_999_999, 0),
            (250_000_000, 0),
            (250_000_001, 1),
            (300_000_000, 50_000_000),
            (u64::MAX, u64::MAX - 250_000_000),
        ] {
            assert_eq!(available_for_claims(mature, 0), expected, "mature={mature}");
        }
    }
}
