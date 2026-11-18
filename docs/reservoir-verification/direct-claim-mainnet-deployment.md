# Direct faucet claims: mainnet bridge deployment

## Deployed runtime

- Source commit: `c5e327d96788b8a0641cd2ee443e98e422f83002`.
- Miner client source commit: `829065d9c9ce820b38342f779f03ac3c3cafe42b`.
- Cargo target: `stratum-bridge`; installed service binary: `/usr/local/bin/stratum-bridge-mainnet`.
- Binary SHA-256: `2ece8ad926bb0de9b0351c64a7b99d1a1179b01ddae5d7bcaba0a4edd1362bfc`.
- Binary size: 50,063,392 bytes.
- Maximum required GLIBC: 2.38; VPS runtime: 2.43.
- Bridge startup: November 18, 2026, 18:28:08 UTC (November 19, 2026, 01:28:08 WIB).
- Bridge PID after upgrade: `1097867`; previous PID: `1088138`.
- Node PID remained `1085691`, with its binary hash and start time unchanged. No chain reset, node restart, or genesis change was performed.

## Release verification

- Bridge tests: 170 library tests passed, 2 ignored; 127 binary tests passed.
- Flutter tests: 229 passed.
- The deployed release binary was exercised by `direct_claim_http_probe` on an isolated simnet before upload. Mainnet storage-mass defaults were retained; simnet bypassed proof of work. The probe verified HTTP claim submission, node mempool admission, receiver UTXOs, an independent second wallet payment, and the same transaction ID after retry and restart without duplicate payment.
- The staged VPS binary passed `--help` and matched the local SHA-256 before replacement.
- After deployment, `/proc/1097867/exe` and the installed binary matched the release SHA-256. The bridge CWD remained `/var/lib/celenium-bridge-mainnet`.
- Local and public API requests returned HTTP 200 JSON. Faucet status reported `claimPayoutMode: on-chain`, `minWithdrawPradha: 1`, and the existing `daily-pool-halving` reward model.
- An empty public claim request returned `EmptyAddress`, `ok: false`, and `claimPayoutMode: on-chain`, with no transaction ID. No mainnet payment was performed for verification.
- Complete ledger entries, device records, and settled withdrawal IDs matched the stopped-service backup. The ledger retained 2 entries and total liabilities of 25,000,001,000 pradha.
- Both services remained active with `NRestarts=0` after startup verification. Startup logs confirmed the existing faucet signer address was enabled.

## Operational limits at verification

- Verified free funds: 0 pradha.
- Mature reservoir funding: 0 pradha.
- Node status: connected to the bridge, still syncing, with no peers reported.
- The former nominal accumulation threshold of 2,500,000 pradha is removed. Actual transaction construction still enforces funding, fees, and consensus transaction-mass limits.
- Deployment proves the direct-claim runtime is live, not that a mainnet recipient has been paid. Funding and blockchain confirmation are still required.
- APK 1.0.10+9010 was built locally. This bridge deployment did not publish or replace website/Drive APK downloads.
- GitHub push was unavailable because the account was suspended. Local commits remain available independently of GitHub.

## Backup and recovery

- VPS backup: `/root/celenium-backups/bridge-c5e327d96788/`.
- Previous binary: `stratum-bridge-mainnet.previous`.
- Previous binary SHA-256: `7dd0bbf4d0a1911a7a45e01037e1cfe9d68fb04de5a09dba11b89aa7c2347ac2`.
- The backup includes a consistent stopped-service copy of `state/`, `faucet_ledger.json`, `config.yaml`, systemd configuration, and `release.json`.
- Verification report: `/root/celenium-backups/bridge-c5e327d96788/direct-live-verification.json`.
- For a binary rollback, stop only `celenium-bridge-mainnet`, install the backed-up binary at its original path, and start the bridge. Inspect current ledger/journal compatibility first; never overwrite newer transaction state with the pre-upgrade ledger automatically. Do not reset the chain or restart the node for a bridge-only rollback.
