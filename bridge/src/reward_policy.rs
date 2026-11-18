pub const INITIAL_MIN_REWARD_PRADHA: u64 = 100_000;
pub const INITIAL_MAX_REWARD_PRADHA: u64 = 500_000;
pub const HALVING_MONTHS: u64 = 48;
pub const REWARD_MODEL: &str = "daily-pool-halving";

pub fn reward_bounds(month: u64) -> (u64, u64) {
    if crate::emission_policy::subsidy_pradha_month(month) == 0 {
        return (0, 0);
    }
    let halvings = month / HALVING_MONTHS;
    if halvings >= u64::BITS as u64 {
        return (0, 0);
    }
    let maximum = INITIAL_MAX_REWARD_PRADHA >> halvings;
    if maximum == 0 {
        return (0, 0);
    }
    ((INITIAL_MIN_REWARD_PRADHA >> halvings).max(1), maximum)
}

pub fn pool_quota(month: u64, budget: u64, expected_claimants: u64) -> u64 {
    let (minimum, maximum) = reward_bounds(month);
    if maximum == 0 || budget < minimum {
        return 0;
    }
    let per_claimant = budget / expected_claimants.max(1);
    if per_claimant < minimum {
        return 0;
    }
    per_claimant.min(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_range_halves_at_each_48_month_boundary() {
        for (month, expected) in [
            (0, (100_000, 500_000)),
            (47, (100_000, 500_000)),
            (48, (50_000, 250_000)),
            (95, (50_000, 250_000)),
            (96, (25_000, 125_000)),
            (144, (12_500, 62_500)),
            (816, (0, 0)),
            (912, (0, 0)),
            (u64::MAX, (0, 0)),
        ] {
            assert_eq!(reward_bounds(month), expected, "month={month}");
        }
    }

    #[test]
    fn pool_quota_shares_budget_and_never_issues_a_partial_minimum() {
        assert_eq!(pool_quota(0, 3_000_000, 1), 500_000);
        assert_eq!(pool_quota(0, 3_000_000, 10), 300_000);
        assert_eq!(pool_quota(0, 3_000_000, 30), 100_000);
        assert_eq!(pool_quota(0, 3_000_000, u64::MAX), 0);
        assert_eq!(pool_quota(0, 99_999, 1), 0);
        assert_eq!(pool_quota(0, 100_000, 0), 100_000);
        assert_eq!(pool_quota(48, 1_500_000, 10), 150_000);
        assert_eq!(pool_quota(912, u64::MAX, 1), 0);
    }
}
