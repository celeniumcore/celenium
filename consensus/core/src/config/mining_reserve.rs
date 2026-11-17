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

#[cfg(test)]
mod tests {
    use super::*;

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
