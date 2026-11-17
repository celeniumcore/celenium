use super::emission::SUBSIDY_BY_MONTH_TABLE;

pub const PRADHA_PER_CEN: u64 = 100_000_000;
pub const MINING_BUDGET: u64 = 52_500_000 * PRADHA_PER_CEN;
pub const INITIAL_RESERVE: u64 = 2_500_000 * PRADHA_PER_CEN;
pub const FAUCET_RESERVE: u64 = 500_000 * PRADHA_PER_CEN;
pub const GENERAL_RESERVE: u64 = INITIAL_RESERVE - FAUCET_RESERVE;
pub const FUTURE_MINING: u64 = MINING_BUDGET - INITIAL_RESERVE;
pub const SECONDS_PER_MONTH: u64 = 2_629_800;
pub const BPS: u64 = 10;
pub const BLOCKS_PER_MONTH: u64 = BPS * SECONDS_PER_MONTH;

const fn legacy_prefix() -> [u64; SUBSIDY_BY_MONTH_TABLE.len() + 1] {
    let mut totals = [0u64; SUBSIDY_BY_MONTH_TABLE.len() + 1];
    let mut month = 0;
    while month < SUBSIDY_BY_MONTH_TABLE.len() {
        totals[month + 1] = totals[month] + SUBSIDY_BY_MONTH_TABLE[month].div_ceil(BPS) * BLOCKS_PER_MONTH;
        month += 1;
    }
    totals
}

const LEGACY_PREFIX: [u64; SUBSIDY_BY_MONTH_TABLE.len() + 1] = legacy_prefix();
pub const END_DAA: u64 = (SUBSIDY_BY_MONTH_TABLE.len() as u64 - 1) * BLOCKS_PER_MONTH;
const FIRST_SUBSIDY: u64 = SUBSIDY_BY_MONTH_TABLE[0].div_ceil(BPS);
const LEGACY_WEIGHT: u64 = LEGACY_PREFIX[SUBSIDY_BY_MONTH_TABLE.len()] - FIRST_SUBSIDY;

/// DAA zero belongs to genesis; only scores from one onward earn mining subsidy.
pub fn cumulative_mining(daa: u64) -> u64 {
    let score = daa.min(END_DAA);
    let month = (score / BLOCKS_PER_MONTH) as usize;
    let partial = score % BLOCKS_PER_MONTH;
    let weight = LEGACY_PREFIX[month] + SUBSIDY_BY_MONTH_TABLE[month].div_ceil(BPS) * partial;
    ((weight.saturating_sub(FIRST_SUBSIDY) as u128 * FUTURE_MINING as u128) / LEGACY_WEIGHT as u128) as u64
}

pub fn mining_subsidy(daa: u64) -> u64 {
    cumulative_mining(daa.saturating_add(1)).saturating_sub(cumulative_mining(daa))
}

#[derive(Clone, Copy, Debug)]
pub struct ActivationSchedule {
    activation: u64,
}

impl ActivationSchedule {
    pub fn new(activation: u64) -> Option<Self> {
        (activation > 0 && activation < 48 * BLOCKS_PER_MONTH).then_some(Self { activation })
    }

    pub fn cumulative(&self, daa: u64) -> u64 {
        let score = daa.min(END_DAA);
        let historical = self.activation * FIRST_SUBSIDY;
        if score <= self.activation {
            return score * FIRST_SUBSIDY;
        }
        let prefix = |at: u64| {
            let month = (at / BLOCKS_PER_MONTH) as usize;
            LEGACY_PREFIX[month] + SUBSIDY_BY_MONTH_TABLE[month].div_ceil(BPS) * (at % BLOCKS_PER_MONTH)
        };
        let remaining_weight = prefix(END_DAA) - prefix(self.activation);
        let remaining_mining = MINING_BUDGET - INITIAL_RESERVE - historical;
        historical + (((prefix(score) - prefix(self.activation)) as u128 * remaining_mining as u128) / remaining_weight as u128) as u64
    }

    pub fn subsidy_at_acceptance_index(&self, index: u64) -> u64 {
        self.cumulative(index.saturating_add(1)).saturating_sub(self.cumulative(index))
    }

    pub fn crosses_activation(&self, start: u64, end: u64) -> bool {
        start < self.activation && end >= self.activation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_preserves_history_and_allocates_reserve_from_the_mining_budget() {
        let schedule = ActivationSchedule::new(1000).unwrap();
        assert_eq!(schedule.cumulative(999), 999 * FIRST_SUBSIDY);
        assert_eq!(schedule.cumulative(1000), 1000 * FIRST_SUBSIDY);
        assert_eq!(schedule.cumulative(END_DAA) + INITIAL_RESERVE, MINING_BUDGET);
        assert!(schedule.crosses_activation(998, 1001));
        assert!(!schedule.crosses_activation(1000, 1001));
        assert!(ActivationSchedule::new(0).is_none());
        assert!(ActivationSchedule::new(48 * BLOCKS_PER_MONTH).is_none());
    }

    #[test]
    fn activation_boundary_counts_only_non_genesis_legacy_rewards() {
        let activation = 1000;
        let schedule = ActivationSchedule::new(activation).unwrap();
        assert_eq!(schedule.cumulative(activation), (activation - 1) * FIRST_SUBSIDY);
    }

    #[test]
    fn partitioned_acceptance_intervals_conserve_emission_across_halvings() {
        let schedule = ActivationSchedule::new(1000).unwrap();
        let mut start = 1000;
        let mut sum = 0;
        for end in [1001, 1007, BLOCKS_PER_MONTH, 48 * BLOCKS_PER_MONTH - 1, 48 * BLOCKS_PER_MONTH + 3, END_DAA - 1, END_DAA] {
            sum += schedule.cumulative(end) - schedule.cumulative(start);
            start = end;
        }
        assert_eq!(sum + 1000 * FIRST_SUBSIDY + INITIAL_RESERVE, MINING_BUDGET);
        let boundary = 48 * BLOCKS_PER_MONTH;
        let siblings_merged = (boundary - 1..boundary + 1).map(|index| schedule.subsidy_at_acceptance_index(index)).sum::<u64>();
        assert_eq!(siblings_merged, schedule.cumulative(boundary + 1) - schedule.cumulative(boundary - 1));
    }

    #[test]
    fn every_month_telescopes_to_fifty_million_without_exceeding_legacy_rewards() {
        let mut total = 0u64;
        for month in 0..SUBSIDY_BY_MONTH_TABLE.len() - 1 {
            let start = month as u64 * BLOCKS_PER_MONTH;
            let end = start + BLOCKS_PER_MONTH;
            total += cumulative_mining(end) - cumulative_mining(start);
            let legacy = SUBSIDY_BY_MONTH_TABLE[month].div_ceil(BPS);
            for offset in [0, 1, 2, 17, BLOCKS_PER_MONTH / 2, BLOCKS_PER_MONTH - 2, BLOCKS_PER_MONTH - 1] {
                let daa = start + offset;
                assert!(mining_subsidy(daa) <= legacy, "month={month} daa={daa}");
                assert!(cumulative_mining(daa + 1) >= cumulative_mining(daa));
            }
        }
        assert_eq!(total, FUTURE_MINING);
        assert!(mining_subsidy(48 * BLOCKS_PER_MONTH) < mining_subsidy(48 * BLOCKS_PER_MONTH - 1));
    }

    #[test]
    fn reserve_and_future_mining_preserve_the_budget_exactly() {
        let end = (SUBSIDY_BY_MONTH_TABLE.len() as u64 - 1) * BLOCKS_PER_MONTH;
        assert_eq!(cumulative_mining(end), FUTURE_MINING);
        assert_eq!(cumulative_mining(end) + INITIAL_RESERVE, MINING_BUDGET);
        assert_eq!(GENERAL_RESERVE + FAUCET_RESERVE, INITIAL_RESERVE);
        assert_eq!(MINING_BUDGET + 16_500_000 * PRADHA_PER_CEN, 69_000_000 * PRADHA_PER_CEN);
        assert_eq!(mining_subsidy(0), 0);
        assert_eq!(mining_subsidy(end), 0);
        assert_eq!(mining_subsidy(u64::MAX), 0);
    }
}
