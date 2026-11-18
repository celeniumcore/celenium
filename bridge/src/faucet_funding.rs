use std::sync::Mutex;

use anyhow::{Context, Result, ensure};
use celenium_addresses::{Address, Prefix};

use crate::reservoir_funds::{FaucetFundingSnapshot, PayoutSource, select_withdrawal_source};
use crate::{celeniumapi::CeleniumApi, faucet::FaucetState, faucet_signer::FaucetSigner};

pub fn validate_addresses(operational: &str, reserve: Option<&str>) -> Result<Prefix> {
    let source = Address::try_from(operational).context("invalid operational faucet address")?;
    ensure!(source.to_string() == operational, "noncanonical operational faucet address");
    if let Some(reserve) = reserve {
        let backup = Address::try_from(reserve).context("invalid faucet reserve address")?;
        ensure!(backup.to_string() == reserve, "noncanonical faucet reserve address");
        ensure!(source != backup, "operational and reserve addresses must differ");
        ensure!(source.prefix == backup.prefix, "operational and reserve networks must match");
    }
    Ok(source.prefix)
}

/// Caller holds FAUCET_WITHDRAW_LOCK across refresh and any claim or transfer.
pub async fn refresh(
    api: &CeleniumApi,
    state: &Mutex<FaucetState>,
    operational: &str,
    reserve: Option<&str>,
) -> Result<FaucetFundingSnapshot> {
    state.lock().unwrap().invalidate_funding();
    validate_addresses(operational, reserve)?;
    let journal = crate::treasury::TreasuryJournal::load(std::path::Path::new(crate::treasury::TREASURY_JOURNAL_PATH))?;
    ensure!(journal.can_prepare(), "reservoir transfer pending");
    let funds = tokio::time::timeout(std::time::Duration::from_secs(10), api.faucet_funding_snapshot(operational, reserve))
        .await
        .context("faucet funding query timed out")??;
    let mut state = state.lock().unwrap();
    ensure!(!state.storage_uncertain, "faucet ledger requires recovery");
    state.set_funding_address(operational)?;
    ensure!(state.update_funding_sources(funds, FaucetState::now_secs()), "faucet funding snapshot is stale or incomplete");
    if state.funding_snapshot_dirty {
        if let Err(error) = state.save(std::path::Path::new(crate::faucet::FAUCET_LEDGER_PATH)) {
            state.storage_uncertain = true;
            return Err(error).context("cannot persist the daily funding budget");
        }
    }
    Ok(funds)
}

/// Prepares one source only; callers journal the returned signed transaction before submitting.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_withdrawal(
    api: &CeleniumApi,
    state: &Mutex<FaucetState>,
    operational: &str,
    operational_signer: Option<&FaucetSigner>,
    reserve: Option<(&str, &FaucetSigner)>,
    destination: &str,
    amount: u64,
    simnet_storage_mass_floor: Option<u64>,
) -> Result<(PayoutSource, crate::transfer::PreparedTransfer)> {
    validate_addresses(operational, reserve.map(|(address, _)| address))?;
    if let Some(signer) = operational_signer {
        signer.ensure_matches(operational)?;
    }
    if let Some((address, signer)) = reserve {
        signer.ensure_matches(address)?;
    }
    let target = Address::try_from(destination)?;
    ensure!(
        target.to_string() == destination && target.prefix == Address::try_from(operational)?.prefix,
        "withdrawal network mismatch"
    );
    ensure!(
        destination != operational && reserve.is_none_or(|(address, _)| destination != address),
        "cannot withdraw to a funding source"
    );
    {
        let state = state.lock().unwrap();
        state.check_withdraw(destination)?;
        ensure!(amount >= crate::faucet::FAUCET_MIN_WITHDRAW_PRADHA, "withdrawal below minimum");
        ensure!(amount <= state.balance(destination).map(|entry| entry.balance_pradha).unwrap_or(0), "withdrawal exceeds balance");
    }
    let funds = refresh(api, state, operational, reserve.map(|(address, _)| address)).await?;
    let mut last_error = anyhow::anyhow!("InsufficientBacking: no single funded signer covers this withdrawal");
    for (source, address, signer) in [
        (PayoutSource::Operational, operational, operational_signer),
        (PayoutSource::Reserve, reserve.map(|(address, _)| address).unwrap_or(""), reserve.map(|(_, signer)| signer)),
    ] {
        let Some(signer) = signer else { continue };
        let mature = match source {
            PayoutSource::Operational => funds.operational.mature_pradha,
            PayoutSource::Reserve => funds.reserve.map(|f| f.mature_pradha).unwrap_or(0),
        };
        if mature <= amount {
            continue;
        }
        let quote = match simnet_storage_mass_floor {
            Some(floor) => api.prepare_simnet_transfer(address, destination, amount, signer.secret_hex(), floor).await,
            None => api.prepare_transfer(address, destination, amount, signer.secret_hex()).await,
        };
        let prepared = match quote {
            Ok(prepared) => prepared,
            Err(error) => {
                last_error = error;
                continue;
            }
        };
        let state = state.lock().unwrap();
        let now = FaucetState::now_secs();
        let combined = state.funding.filter(|f| f.is_fresh(now)).context("faucet funding expired during quote")?;
        let fee = prepared.fee_pradha();
        if let Err(error) = state.check_withdraw_fee(destination, fee) {
            last_error = error.into();
            continue;
        }
        let selected = select_withdrawal_source(
            (source == PayoutSource::Operational).then_some(fee),
            (source == PayoutSource::Reserve).then_some(fee),
            amount,
            state.total_outstanding(),
            funds.operational.mature_pradha,
            combined.mature_pradha.saturating_sub(funds.operational.mature_pradha),
            combined.mature_pradha,
        );
        if selected == Some(source) {
            return Ok((source, prepared));
        }
    }
    Err(last_error)
}

/// Signs a candidate claim without persisting its new credit or broadcasting.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_claim_payment(
    api: &CeleniumApi,
    state: &FaucetState,
    operational: &str,
    operational_signer: Option<&FaucetSigner>,
    reserve: Option<(&str, &FaucetSigner)>,
    destination: &str,
    amount: u64,
) -> Result<(PayoutSource, crate::transfer::PreparedTransfer)> {
    validate_addresses(operational, reserve.map(|(address, _)| address))?;
    let target = Address::try_from(destination)?;
    ensure!(target.to_string() == destination && target.prefix == Address::try_from(operational)?.prefix, "claim network mismatch");
    ensure!(
        destination != operational && reserve.is_none_or(|(address, _)| destination != address),
        "cannot claim to a funding source"
    );
    state.check_withdraw(destination)?;
    ensure!(
        amount > 0 && amount <= state.balance(destination).map(|entry| entry.balance_pradha).unwrap_or(0),
        "claim exceeds balance"
    );
    let now = FaucetState::now_secs();
    let funds = state.funding_sources.context("FundingUnavailable: no verified source snapshot")?;
    let combined = state.funding.filter(|funding| funding.is_fresh(now)).context("FundingUnavailable: expired funding")?;
    let mut last = anyhow::anyhow!("FundingUnavailable: no funded signer covers this claim");
    for (source, address, signer, mature) in [
        (PayoutSource::Operational, operational, operational_signer, funds.operational.mature_pradha),
        (
            PayoutSource::Reserve,
            reserve.map(|(address, _)| address).unwrap_or(""),
            reserve.map(|(_, signer)| signer),
            funds.reserve.map(|funding| funding.mature_pradha).unwrap_or(0),
        ),
    ] {
        let Some(signer) = signer else { continue };
        signer.ensure_matches(address)?;
        if mature <= amount {
            continue;
        }
        let quote = match api.prepare_transfer(address, destination, amount, signer.secret_hex()).await {
            Ok(quote) => quote,
            Err(error) => {
                last = error;
                continue;
            }
        };
        let fee = quote.fee_pradha();
        state.check_withdraw_fee(destination, fee)?;
        let selected = select_withdrawal_source(
            (source == PayoutSource::Operational).then_some(fee),
            (source == PayoutSource::Reserve).then_some(fee),
            amount,
            state.total_outstanding(),
            funds.operational.mature_pradha,
            combined.mature_pradha.saturating_sub(funds.operational.mature_pradha),
            combined.mature_pradha,
        );
        if selected == Some(source) {
            ensure!(combined.is_fresh(FaucetState::now_secs()), "FundingUnavailable: funding expired during quote");
            return Ok((source, quote));
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use celenium_addresses::{Address, Prefix, Version};

    #[test]
    fn reserve_cannot_alias_operational_address_or_use_another_network() {
        let operational = Address::new(Prefix::Simnet, Version::PubKey, &[1; 32]).to_string();
        let reserve = Address::new(Prefix::Simnet, Version::PubKey, &[2; 32]).to_string();
        let other = Address::new(Prefix::Mainnet, Version::PubKey, &[2; 32]).to_string();
        assert!(validate_addresses(&operational, Some(&reserve)).is_ok());
        assert!(validate_addresses(&operational, None).is_ok());
        assert!(validate_addresses(&operational, Some(&operational)).is_err());
        assert!(validate_addresses(&operational, Some(&other)).is_err());
        assert!(validate_addresses(&operational, Some(&format!(" {reserve}"))).is_err());
    }
}
