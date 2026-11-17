#!/usr/bin/env python3
"""Exact integer allocation model for a local opt-in Celenium new genesis."""

import argparse
import csv
import hashlib
import json
from pathlib import Path
import re
import sys


SOURCE_PATHS = (
    "consensus/core/src/config/emission.rs",
    "consensus/src/processes/coinbase.rs",
)

PRADHA_PER_CEN = 100_000_000
BPS = 10
SECONDS_PER_MONTH = 2_629_800
HALVING_MONTHS = 48
MINING_NOMINAL = 52_500_000 * PRADHA_PER_CEN
RESERVE = 2_500_000 * PRADHA_PER_CEN
FAUCET_RESERVE = 500_000 * PRADHA_PER_CEN
FUTURE_MINING = MINING_NOMINAL - RESERVE
PREMINE_DEV = 9_600_000 * PRADHA_PER_CEN
PREMINE_OWNER = 6_900_000 * PRADHA_PER_CEN
FINAL_NOMINAL = 69_000_000 * PRADHA_PER_CEN


def require_index(value: int) -> None:
    if type(value) is not int or value < 0:
        raise ValueError("DAA score and month must be nonnegative integers")


class EmissionSchedule:
    def __init__(self, rates: tuple[int, ...]):
        self.rates = rates
        self.blocks_per_month = BPS * SECONDS_PER_MONTH
        self.rewards = tuple((rate + BPS - 1) // BPS for rate in rates)
        self.end_daa = (len(rates) - 1) * self.blocks_per_month
        self.prefix = [0]
        for reward in self.rewards:
            self.prefix.append(self.prefix[-1] + reward * self.blocks_per_month)
        self.legacy_total = self.prefix[-1]
        self.offset = self.rewards[0]
        self.denominator = self.legacy_total - self.offset

    def legacy_prefix(self, n: int) -> int:
        """W(n): legacy rewards at DAA indices [0, n), clamped at emission end."""
        require_index(n)
        month, blocks = divmod(min(n, self.end_daa), self.blocks_per_month)
        return self.prefix[month] + self.rewards[month] * blocks

    def legacy_reward(self, daa: int) -> int:
        require_index(daa)
        return self.rewards[min(daa // self.blocks_per_month, len(self.rewards) - 1)]

    def cumulative(self, n: int) -> int:
        """Q(n): new future mining at indices [1, n), excluding the genesis reserve."""
        return max(0, self.legacy_prefix(n) - self.offset) * FUTURE_MINING // self.denominator

    def reward(self, daa: int) -> int:
        require_index(daa)
        if daa == 0 or daa >= self.end_daa:
            return 0
        return self.cumulative(daa + 1) - self.cumulative(daa)

    def month_rows(self) -> list[dict[str, int]]:
        rows = []
        for month, old_reward in enumerate(self.rewards):
            start = month * self.blocks_per_month
            stop = start + self.blocks_per_month
            eligible_start = max(1, start)
            blocks = stop - eligible_start
            emitted = self.cumulative(stop) - self.cumulative(eligible_start)
            low, remainder = divmod(old_reward * FUTURE_MINING, self.denominator)
            high = low + (remainder != 0)
            # Floor differences are low or high; endpoint sums determine their counts.
            high_count = emitted - low * blocks if remainder else 0
            floor, faucet = self.reserve_targets(month)
            rows.append({
                "month": month,
                "start_daa": start,
                "stop_daa_exclusive": stop,
                "eligible_start_daa": eligible_start,
                "scheduled_blocks": blocks,
                "legacy_rate_pradha_per_second": self.rates[month],
                "legacy_reward_pradha": old_reward,
                "legacy_month_pradha": old_reward * self.blocks_per_month,
                "reward_floor_pradha": low,
                "reward_ceil_pradha": high,
                "floor_reward_blocks": blocks - high_count,
                "ceil_reward_blocks": high_count,
                "future_mining_pradha": emitted,
                "cumulative_future_pradha": self.cumulative(stop),
                "target_floor_pradha": floor,
                "faucet_target_pradha": faucet,
            })
        return rows

    def reserve_targets(self, month: int) -> tuple[int, int]:
        """Policy targets at the start of a legacy month, not on-chain balances."""
        require_index(month)
        remaining = self.legacy_total - self.legacy_prefix(month * self.blocks_per_month)
        floor = RESERVE * remaining // self.legacy_total
        return floor, min(floor, FAUCET_RESERVE)


def parse_legacy_table(text: str) -> tuple[int, ...]:
    text = re.sub(r"/\*.*?\*/|//[^\n]*", "", text, flags=re.S)
    match = re.search(
        r"\bconst\s+SUBSIDY_BY_MONTH_TABLE\s*:\s*\[u64;\s*817\s*\]"
        r"\s*=\s*\[(.*?)\];", text, re.S,
    )
    if match is None:
        raise ValueError("expected SUBSIDY_BY_MONTH_TABLE: [u64; 817]")
    body = match[1].strip().removesuffix(",")
    tokens = [value.strip() for value in body.split(",")]
    if any(re.fullmatch(r"[0-9]+(?:_[0-9]+)*", token) is None for token in tokens):
        raise ValueError("subsidy table must contain unsigned integer literals")
    rates = tuple(int(token.replace("_", "")) for token in tokens)
    if len(rates) != 817 or rates[-1] != 0 or any(not 0 < rate < 2**64 for rate in rates[:-1]):
        raise ValueError("expected 816 positive u64 rates and one trailing zero")
    for start in range(0, len(rates) - 1, HALVING_MONTHS):
        if any(rate != rates[start] for rate in rates[start:start + HALVING_MONTHS]):
            raise ValueError("legacy rates must stay constant inside each 48-month era")
        if start and abs(2 * rates[start] - rates[start - HALVING_MONTHS]) > 2:
            raise ValueError("legacy rates must follow nominal 48-month halvings")
    return rates


def load_legacy_table(root: Path) -> tuple[Path, tuple[int, ...]]:
    for relative in SOURCE_PATHS:
        source = root / relative
        if source.is_file():
            return source, parse_legacy_table(source.read_text(encoding="utf-8"))
    raise ValueError("legacy SUBSIDY_BY_MONTH_TABLE not found in repository")


def format_cen(pradha: int) -> str:
    whole, fraction = divmod(abs(pradha), PRADHA_PER_CEN)
    return f"{'-' if pradha < 0 else ''}{whole}.{fraction:08d}"


def build_report(root: Path) -> dict:
    source, rates = load_legacy_table(root)
    schedule = EmissionSchedule(rates)
    rows = schedule.month_rows()
    premine = PREMINE_DEV + PREMINE_OWNER
    allocation = {
        "original_mining_nominal": MINING_NOMINAL,
        "general_reserve_at_genesis": RESERVE - FAUCET_RESERVE,
        "faucet_reserve_at_genesis": FAUCET_RESERVE,
        "initial_mining_reserve": RESERVE,
        "future_mining": FUTURE_MINING,
        "cltv_dev_premine": PREMINE_DEV,
        "cltv_owner_premine": PREMINE_OWNER,
        "cltv_premine_unchanged": premine,
        "total_at_new_genesis": premine + RESERVE,
        "final_nominal_supply": FINAL_NOMINAL,
    }
    one_bps_total = sum(rates) * SECONDS_PER_MONTH
    legacy = {
        "actual_10_bps_weighted_total": schedule.legacy_total,
        "deviation_from_nominal": schedule.legacy_total - MINING_NOMINAL,
        "one_bps_table_total": one_bps_total,
        "one_bps_table_deviation": one_bps_total - MINING_NOMINAL,
        "ten_bps_ceiling_increase": schedule.legacy_total - one_bps_total,
        "total_with_cltv_premine": schedule.legacy_total + premine,
        "excluded_daa_zero_weight": schedule.offset,
        "future_weight_denominator": schedule.denominator,
    }
    interval_sum = sum(row["future_mining_pradha"] for row in rows)
    checks = {
        "exact_future_total": interval_sum == FUTURE_MINING == schedule.cumulative(schedule.end_daa),
        "reserve_plus_future_is_nominal_mining": RESERVE + interval_sum == MINING_NOMINAL,
        "genesis_plus_future_is_final_nominal": premine + RESERVE + interval_sum == FINAL_NOMINAL,
        "zero_at_genesis_and_after_end": schedule.reward(0) == schedule.reward(schedule.end_daa) == 0,
        "scale_at_most_one": 0 < FUTURE_MINING <= schedule.denominator,
        "no_reward_increase": all(row["reward_ceil_pradha"] <= row["legacy_reward_pradha"] for row in rows),
        "monthly_histograms_match": all(
            row["floor_reward_blocks"] >= 0 and row["ceil_reward_blocks"] >= 0
            and row["floor_reward_blocks"] + row["ceil_reward_blocks"] == row["scheduled_blocks"]
            and row["reward_floor_pradha"] * row["floor_reward_blocks"]
            + row["reward_ceil_pradha"] * row["ceil_reward_blocks"] == row["future_mining_pradha"]
            for row in rows
        ),
        "nominal_48_month_eras": all(
            rates[month] == rates[(month // HALVING_MONTHS) * HALVING_MONTHS]
            for month in range(len(rates) - 1)
        ),
    }
    if not all(checks.values()):
        raise ValueError(f"allocation verification failed: {checks}")
    return {
        "scope": "local-opt-in-new-genesis",
        "live_funds_verified": False,
        "notice": "Model lokal; alokasi genesis baru mengubah hash genesis. Bukan saldo atau aktivasi mainnet.",
        "source": {
            "path": source.relative_to(root).as_posix(),
            "table_entries": len(rates),
            "table_sha256": hashlib.sha256(",".join(map(str, rates)).encode("ascii")).hexdigest(),
        },
        "parameters": {
            "pradha_per_cen": PRADHA_PER_CEN,
            "bps": BPS,
            "seconds_per_month": SECONDS_PER_MONTH,
            "blocks_per_month": schedule.blocks_per_month,
            "nominal_halving_months": HALVING_MONTHS,
            "emission_end_daa_exclusive": schedule.end_daa,
            "first_future_reward_daa": 1,
        },
        "allocation_pradha": allocation,
        "allocation_cen": {name: format_cen(value) for name, value in allocation.items()},
        "legacy_pradha": legacy,
        "legacy_cen": {name: format_cen(value) for name, value in legacy.items()},
        "verification": {"interval_sum_pradha": interval_sum, "checks": checks},
        "months": rows,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    output = parser.add_mutually_exclusive_group()
    output.add_argument("--json", action="store_true", help="print the full JSON report (default)")
    output.add_argument("--csv", action="store_true", help="print monthly intervals as CSV to stdout")
    args = parser.parse_args()
    report = build_report(Path(__file__).resolve().parents[1])
    if args.csv:
        rows = report["months"]
        writer = csv.DictWriter(sys.stdout, fieldnames=list(rows[0]))
        writer.writeheader()
        writer.writerows(rows)
    else:
        print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
