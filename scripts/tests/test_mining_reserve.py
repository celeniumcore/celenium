"""Run with python3 -B -m unittest discover -s scripts/tests -p test_mining_reserve.py."""

import importlib.util
import csv
import io
import json
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/calculate-mining-reserve.py"
ERA_RATES = (
    20_795_466, 10_397_733, 5_198_867, 2_599_433, 1_299_717,
    649_858, 324_929, 162_465, 81_232, 40_616, 20_308, 10_154,
    5_077, 2_539, 1_269, 635, 317,
)
EXPECTED_RATES = tuple(rate for rate in ERA_RATES for _ in range(48)) + (0,)


class MiningReserveTests(unittest.TestCase):
    def calculator(self):
        self.assertTrue(SCRIPT.is_file(), "mining reserve calculator is missing")
        spec = importlib.util.spec_from_file_location("mining_reserve", SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    def test_loads_exact_legacy_table_from_repository(self):
        calc = self.calculator()
        source, rates = calc.load_legacy_table(ROOT)
        self.assertIn(source.relative_to(ROOT).as_posix(), (
            "consensus/core/src/config/emission.rs",
            "consensus/src/processes/coinbase.rs",
        ))
        self.assertEqual(rates, EXPECTED_RATES)
        self.assertEqual(len(rates), 817)
        self.assertEqual(sum((rate + 9) // 10 for rate in rates) * 26_298_000,
                         5_250_008_172_672_000)

    def test_exact_schedule_boundaries_exclude_genesis_weight(self):
        calc = self.calculator()
        self.assertTrue(hasattr(calc, "EmissionSchedule"), "exact schedule is missing")
        _, rates = calc.load_legacy_table(ROOT)
        schedule = calc.EmissionSchedule(rates)
        self.assertEqual(schedule.blocks_per_month, 26_298_000)
        self.assertEqual(schedule.end_daa, 21_459_168_000)
        self.assertEqual(schedule.legacy_total, 5_250_008_172_672_000)
        self.assertEqual(schedule.offset, 2_079_547)
        self.assertEqual(schedule.denominator, 5_250_008_170_592_453)
        # Values independently evaluated from the source table and integer formula.
        vectors = (
            (0, 0, 0, 0),
            (1, 2_079_547, 0, 1_980_517),
            (2, 4_159_094, 1_980_517, 1_980_518),
            (26_297_999, 54_687_924_926_453, 52_083_654_986_706, 1_980_518),
            (26_298_000, 54_687_927_006_000, 52_083_656_967_224, 1_980_518),
            (26_298_001, 54_687_929_085_547, 52_083_658_947_742, 1_980_518),
            (1_262_303_999, 2_625_020_494_208_453, 2_500_015_625_530_614, 1_980_518),
            (1_262_304_000, 2_625_020_496_288_000, 2_500_015_627_511_132, 990_260),
            (1_262_304_001, 2_625_020_497_327_774, 2_500_015_628_501_392, 990_259),
            (2_524_608_000, 3_937_531_375_584_000, 3_750_024_043_353_165, 495_130),
            (21_432_870_000, 5_250_007_331_136_000, 4_999_999_198_538_390, 30),
            (21_459_167_998, 5_250_008_172_671_936, 4_999_999_999_999_939, 30),
            (21_459_167_999, 5_250_008_172_671_968, 4_999_999_999_999_969, 31),
            (21_459_168_000, 5_250_008_172_672_000, 5_000_000_000_000_000, 0),
            (21_485_466_000, 5_250_008_172_672_000, 5_000_000_000_000_000, 0),
        )
        for daa, weight, emitted, reward in vectors:
            with self.subTest(daa=daa):
                self.assertEqual(schedule.legacy_prefix(daa), weight)
                self.assertEqual(schedule.cumulative(daa), emitted)
                self.assertEqual(schedule.reward(daa), reward)
                self.assertLessEqual(reward, schedule.legacy_reward(daa))

    def test_month_intervals_prove_conservation_and_per_block_cap(self):
        calc = self.calculator()
        schedule = calc.EmissionSchedule(calc.load_legacy_table(ROOT)[1])
        self.assertTrue(hasattr(schedule, "month_rows"), "interval certificate is missing")
        rows = schedule.month_rows()
        self.assertEqual(len(rows), 817)
        self.assertEqual(rows[0]["eligible_start_daa"], 1)
        self.assertEqual(rows[0]["scheduled_blocks"], 26_297_999)
        self.assertEqual(rows[0]["future_mining_pradha"], 52_083_656_967_224)
        self.assertEqual(rows[48]["start_daa"], 1_262_304_000)
        self.assertEqual(rows[816]["future_mining_pradha"], 0)
        self.assertEqual(sum(row["future_mining_pradha"] for row in rows),
                         5_000_000_000_000_000)
        cumulative = 0
        for month, row in enumerate(rows):
            with self.subTest(month=month):
                self.assertEqual(row["month"], month)
                self.assertEqual(row["start_daa"], month * 26_298_000)
                self.assertEqual(row["stop_daa_exclusive"], (month + 1) * 26_298_000)
                self.assertEqual(row["legacy_reward_pradha"], (EXPECTED_RATES[month] + 9) // 10)
                self.assertEqual(row["scheduled_blocks"], row["floor_reward_blocks"] + row["ceil_reward_blocks"])
                self.assertGreaterEqual(row["floor_reward_blocks"], 0)
                self.assertGreaterEqual(row["ceil_reward_blocks"], 0)
                self.assertLessEqual(row["reward_ceil_pradha"], row["legacy_reward_pradha"])
                self.assertEqual(
                    row["future_mining_pradha"],
                    row["reward_floor_pradha"] * row["floor_reward_blocks"]
                    + row["reward_ceil_pradha"] * row["ceil_reward_blocks"],
                )
                cumulative += row["future_mining_pradha"]
                self.assertEqual(row["cumulative_future_pradha"], cumulative)
                for daa in (max(1, row["start_daa"]), row["stop_daa_exclusive"] - 1):
                    reward = schedule.reward(daa)
                    self.assertLessEqual(row["reward_floor_pradha"], reward)
                    self.assertLessEqual(reward, row["reward_ceil_pradha"])

    def test_reserve_targets_follow_remaining_legacy_at_month_boundaries(self):
        calc = self.calculator()
        schedule = calc.EmissionSchedule(calc.load_legacy_table(ROOT)[1])
        self.assertTrue(hasattr(schedule, "reserve_targets"), "reserve targets are missing")
        vectors = (
            (0, 250_000_000_000_000, 50_000_000_000_000),
            (1, 247_395_817_053_644, 50_000_000_000_000),
            (48, 124_999_218_574_930, 50_000_000_000_000),
            (96, 62_498_797_807_585, 50_000_000_000_000),
            (144, 31_248_587_423_913, 31_248_587_423_913),
            (768, 1_923_507_862, 1_923_507_862),
            (815, 40_073_080, 40_073_080),
            (816, 0, 0),
            (817, 0, 0),
        )
        for month, floor, faucet in vectors:
            with self.subTest(month=month):
                self.assertEqual(schedule.reserve_targets(month), (floor, faucet))
        previous = 250_000_000_000_000
        for month, row in enumerate(schedule.month_rows()):
            floor, faucet = schedule.reserve_targets(month)
            numerator = 250_000_000_000_000 * sum(
                (rate + 9) // 10 for rate in EXPECTED_RATES[month:]) * 26_298_000
            self.assertLessEqual(floor * schedule.legacy_total, numerator)
            self.assertLess(numerator, (floor + 1) * schedule.legacy_total)
            self.assertLessEqual(floor, previous)
            self.assertEqual(faucet, min(floor, 50_000_000_000_000))
            self.assertEqual(row["target_floor_pradha"], floor)
            self.assertEqual(row["faucet_target_pradha"], faucet)
            previous = floor

    def test_cli_json_reports_allocation_and_verified_totals(self):
        result = subprocess.run([sys.executable, "-B", str(SCRIPT)],
                                cwd=ROOT.parent, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.strip(), "JSON allocation report is missing")
        report = json.loads(result.stdout)
        self.assertEqual(report["scope"], "local-opt-in-new-genesis")
        self.assertFalse(report["live_funds_verified"])
        self.assertEqual(report["parameters"]["nominal_halving_months"], 48)
        self.assertEqual(report["source"]["table_entries"], 817)
        self.assertEqual(report["source"]["table_sha256"],
                         "30fc40c35ba310e14cbe98eb0e1914d1ff09330a9133e5f978005eb1c7da9a66")
        allocation = report["allocation_pradha"]
        self.assertEqual(allocation, {
            "original_mining_nominal": 5_250_000_000_000_000,
            "general_reserve_at_genesis": 200_000_000_000_000,
            "faucet_reserve_at_genesis": 50_000_000_000_000,
            "initial_mining_reserve": 250_000_000_000_000,
            "future_mining": 5_000_000_000_000_000,
            "cltv_dev_premine": 960_000_000_000_000,
            "cltv_owner_premine": 690_000_000_000_000,
            "cltv_premine_unchanged": 1_650_000_000_000_000,
            "total_at_new_genesis": 1_900_000_000_000_000,
            "final_nominal_supply": 6_900_000_000_000_000,
        })
        self.assertEqual(allocation["total_at_new_genesis"] + allocation["future_mining"],
                         allocation["final_nominal_supply"])
        legacy = report["legacy_pradha"]
        self.assertEqual(legacy["actual_10_bps_weighted_total"], 5_250_008_172_672_000)
        self.assertEqual(legacy["deviation_from_nominal"], 8_172_672_000)
        self.assertEqual(legacy["one_bps_table_deviation"], -32_304_000)
        self.assertEqual(legacy["ten_bps_ceiling_increase"], 8_204_976_000)
        self.assertEqual(legacy["excluded_daa_zero_weight"], 2_079_547)
        self.assertEqual(legacy["future_weight_denominator"], 5_250_008_170_592_453)
        self.assertEqual(report["legacy_cen"]["actual_10_bps_weighted_total"], "52500081.72672000")
        self.assertEqual(report["legacy_cen"]["deviation_from_nominal"], "81.72672000")
        self.assertEqual(report["legacy_cen"]["one_bps_table_deviation"], "-0.32304000")
        self.assertEqual(len(report["months"]), 817)
        self.assertEqual(report["verification"]["interval_sum_pradha"], 5_000_000_000_000_000)
        self.assertTrue(all(report["verification"]["checks"].values()))

    def test_parser_rejects_malformed_or_non_halving_tables(self):
        calc = self.calculator()
        self.assertTrue(hasattr(calc, "parse_legacy_table"), "strict source parser is missing")

        def source(values):
            return "pub const SUBSIDY_BY_MONTH_TABLE: [u64; 817] = [" + ",".join(map(str, values)) + "];"

        self.assertEqual(calc.parse_legacy_table(source(EXPECTED_RATES)), EXPECTED_RATES)
        self.assertEqual(calc.parse_legacy_table(source(EXPECTED_RATES).replace(
            "20795466", "20_795_466 /* source units */")), EXPECTED_RATES)
        malformed = (
            "// no subsidy table",
            source(EXPECTED_RATES[:-1]),
            source(EXPECTED_RATES).replace("[u64; 817]", "[u64; 816]"),
            source(EXPECTED_RATES).replace("20795466", "20__795466", 1),
            source(EXPECTED_RATES).replace("20795466", "-1", 1),
            source((20_795_465,) + EXPECTED_RATES[1:]),
            source(EXPECTED_RATES[:-1] + (1,)),
            source((0,) + EXPECTED_RATES[1:]),
            source(tuple(2**64 for _ in range(816)) + (0,)),
        )
        for text in malformed:
            with self.subTest(source_start=text[:80]):
                with self.assertRaises(ValueError):
                    calc.parse_legacy_table(text)

    def test_cli_json_and_csv_modes_match(self):
        default = subprocess.run([sys.executable, "-B", str(SCRIPT), "--json"],
                                 cwd=ROOT.parent, text=True, capture_output=True)
        self.assertEqual(default.returncode, 0, default.stderr)
        report = json.loads(default.stdout)
        csv_result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--csv"],
                                    cwd=ROOT.parent, text=True, capture_output=True)
        self.assertEqual(csv_result.returncode, 0, csv_result.stderr)
        rows = list(csv.DictReader(io.StringIO(csv_result.stdout)))
        self.assertEqual(len(rows), 817, "CSV must contain all monthly intervals")
        self.assertEqual([{name: int(value) for name, value in row.items()} for row in rows],
                         report["months"])
        invalid = subprocess.run([sys.executable, "-B", str(SCRIPT), "--json", "--csv"],
                                 cwd=ROOT.parent, text=True, capture_output=True)
        self.assertEqual(invalid.returncode, 2)

    def test_schedule_rejects_negative_or_noninteger_indices(self):
        calc = self.calculator()
        schedule = calc.EmissionSchedule(calc.load_legacy_table(ROOT)[1])
        for method in (schedule.legacy_prefix, schedule.legacy_reward,
                       schedule.cumulative, schedule.reward, schedule.reserve_targets):
            for invalid in (-1, 1.0, True, "1"):
                with self.subTest(method=method.__name__, invalid=invalid):
                    with self.assertRaises(ValueError):
                        method(invalid)
        self.assertEqual(schedule.legacy_prefix(10**40), schedule.legacy_total)
        self.assertEqual(schedule.cumulative(10**40), 5_000_000_000_000_000)
        self.assertEqual(schedule.reward(10**40), 0)
        self.assertEqual(schedule.legacy_reward(10**40), 0)
        self.assertEqual(schedule.reserve_targets(10**40), (0, 0))


if __name__ == "__main__":
    unittest.main()
