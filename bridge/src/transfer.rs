use anyhow::{Context, Result, bail, ensure};
use celenium_addresses::{Address, Version};
use celenium_consensus_core::{
    config::params::Params,
    constants::{MAX_TX_IN_SEQUENCE_NUM, TX_VERSION},
    mass::MassCalculator,
    sign,
    subnets::SUBNETWORK_ID_NATIVE,
    tx::{SignableTransaction, Transaction, TransactionInput, TransactionOutpoint, TransactionOutput, UtxoEntry},
};
use celenium_txscript::pay_to_address_script;

const MAX_TRANSFER_INPUTS: usize = 128;
pub(crate) const FALLBACK_FEERATE: f64 = 0.25;

/// A signed quote. Preparing does not broadcast or reserve its inputs.
#[derive(Clone, Debug)]
pub struct PreparedTransfer {
    pub(crate) transaction: celenium_rpc_core::RpcTransaction,
    pub(crate) network_id: celenium_consensus_core::network::NetworkId,
    input_entries: Vec<celenium_rpc_core::RpcUtxoEntry>,
    fee_pradha: u64,
    transaction_id: String,
}

impl PreparedTransfer {
    pub(crate) fn new(signed: SignableTransaction, params: &Params) -> Result<Self> {
        let inputs = signed.entries.iter().try_fold(0u64, |sum, entry| {
            sum.checked_add(entry.as_ref().context("missing input entry")?.amount).context("input total overflows")
        })?;
        let outputs =
            signed.tx.outputs.iter().try_fold(0u64, |sum, output| sum.checked_add(output.value).context("output total overflows"))?;
        let fee_pradha = inputs.checked_sub(outputs).context("outputs exceed inputs")?;
        let input_entries = signed
            .entries
            .iter()
            .map(|entry| entry.as_ref().context("missing input entry").map(|entry| entry.clone().into()))
            .collect::<Result<Vec<celenium_rpc_core::RpcUtxoEntry>>>()?;
        Ok(Self {
            transaction: (&signed.tx).into(),
            network_id: params.net,
            input_entries,
            fee_pradha,
            transaction_id: signed.tx.id().to_string(),
        })
    }

    pub fn input_entries(&self) -> Vec<celenium_rpc_core::RpcUtxoEntry> {
        self.input_entries.clone()
    }

    pub fn fee_pradha(&self) -> u64 {
        self.fee_pradha
    }

    pub fn transaction_id(&self) -> &str {
        &self.transaction_id
    }

    pub fn transaction_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&self.transaction)?)
    }
}

pub(crate) fn network_params(network: celenium_consensus_core::network::NetworkId) -> Result<Params> {
    use celenium_consensus_core::network::NetworkType;
    ensure!(
        match network.network_type {
            NetworkType::Testnet => network.suffix == Some(10),
            _ => network.suffix.is_none(),
        },
        "unsupported network id {network}"
    );
    Ok(network.into())
}

pub(crate) fn standard_transfer_mass(params: &Params) -> u64 {
    let source = Address::new(params.prefix(), Version::PubKey, &[0; 32]);
    let script = pay_to_address_script(&source);
    let tx = Transaction::new(
        TX_VERSION,
        vec![TransactionInput::new(TransactionOutpoint::new(Default::default(), 0), vec![0; 66], MAX_TX_IN_SEQUENCE_NUM, 1)],
        vec![TransactionOutput::new(1, script.clone()), TransactionOutput::new(1, script)],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    MassCalculator::new_with_consensus_params(params).calc_non_contextual_masses(&tx).normalized_max(&params.block_mass_cofactors())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_transfer(
    params: &Params,
    daa_score: u64,
    source: &Address,
    destination: &Address,
    amount: u64,
    fee: Option<u64>,
    feerate: f64,
    secret: &[u8; 32],
    utxos: Vec<(TransactionOutpoint, UtxoEntry)>,
) -> Result<SignableTransaction> {
    validate_request(params, source, destination, amount, secret)?;
    fee_for_mass(0, feerate)?;
    let source_script = pay_to_address_script(source);
    let calculator = MassCalculator::new_with_consensus_params(params);
    let minimum_total = amount.checked_add(fee.unwrap_or(0)).context("amount plus fee overflows")?;
    let mut selected = Vec::new();
    let mut total = 0u64;
    let mut insufficient_fee = None;
    let mut storage_rejected = false;
    for (outpoint, entry) in utxos {
        if entry.script_public_key != source_script || entry.amount == 0 || entry.covenant_id.is_some() {
            continue;
        }
        if entry.is_coinbase
            && !entry.block_daa_score.checked_add(params.coinbase_maturity()).is_some_and(|mature| mature <= daa_score)
        {
            continue;
        }
        if selected.len() >= MAX_TRANSFER_INPUTS.min(params.max_tx_inputs) {
            break;
        }
        total = total.checked_add(entry.amount).context("input total overflows")?;
        selected.push((outpoint, entry));
        if total < minimum_total {
            continue;
        }
        let mut transaction = Transaction::new(
            TX_VERSION,
            selected.iter().map(|(outpoint, _)| TransactionInput::new(*outpoint, vec![0; 66], MAX_TX_IN_SEQUENCE_NUM, 1)).collect(),
            vec![TransactionOutput::new(amount, pay_to_address_script(destination))],
            0,
            SUBNETWORK_ID_NATIVE,
            0,
            vec![],
        );
        // Schnorr scripts have a fixed encoded length, including push and sighash bytes.
        let no_change_fee =
            fee_for_mass(calculator.calc_non_contextual_masses(&transaction).normalized_max(&params.block_mass_cofactors()), feerate)?;
        let spendable = total.checked_sub(amount).context("insufficient funds")?;
        let candidate_fee = match fee {
            Some(fee) => fee,
            None if spendable == no_change_fee => no_change_fee,
            None => {
                transaction.outputs.push(TransactionOutput::new(1, source_script.clone()));
                fee_for_mass(
                    calculator.calc_non_contextual_masses(&transaction).normalized_max(&params.block_mass_cofactors()),
                    feerate,
                )?
            }
        };
        if spendable < candidate_fee {
            continue;
        }
        transaction.outputs.truncate(1);
        let change = spendable.checked_sub(candidate_fee).context("insufficient funds")?;
        if change > 0 {
            transaction.outputs.push(TransactionOutput::new(change, source_script.clone()));
        }
        transaction.finalize();
        let signed = sign::sign_with_multiple_v2(
            SignableTransaction::with_entries(transaction, selected.iter().map(|(_, entry)| entry.clone()).collect()),
            &[*secret],
        )
        .fully_signed()?;
        let mass = calculator.calc_non_contextual_masses(&signed.tx);
        ensure!(
            mass.compute_mass <= params.block_mass_limits.compute && mass.transient_mass <= params.block_mass_limits.transient,
            "transfer exceeds non-contextual mass limits"
        );
        let storage = calculator.calc_contextual_masses(&signed.as_verifiable());
        if !storage.is_some_and(|mass| mass.storage_mass <= params.block_mass_limits.storage) {
            storage_rejected = true;
            continue;
        }
        let required_fee = fee_for_mass(mass.normalized_max(&params.block_mass_cofactors()), feerate)?;
        if candidate_fee < required_fee {
            insufficient_fee.get_or_insert(required_fee);
            continue;
        }
        sign::verify(&signed.as_verifiable())?;
        return Ok(signed);
    }
    if let Some(required_fee) = insufficient_fee {
        bail!("InsufficientFee: provided {}, required {required_fee} pradha", fee.unwrap_or(0));
    }
    if storage_rejected {
        bail!(
            "SmallTransferNotSupported: no selection within {} inputs satisfies storage mass limit {} for amount {amount}",
            MAX_TRANSFER_INPUTS.min(params.max_tx_inputs),
            params.block_mass_limits.storage
        );
    }
    bail!(
        "InsufficientFunds: no mature UTXO selection within {} inputs covers amount and fee (selected {total} pradha)",
        MAX_TRANSFER_INPUTS.min(params.max_tx_inputs)
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_surplus_transfer(
    params: &Params,
    daa_score: u64,
    source: &Address,
    destination: &Address,
    max_amount: u64,
    feerate: f64,
    secret: &[u8; 32],
    utxos: Vec<(TransactionOutpoint, UtxoEntry)>,
) -> Result<SignableTransaction> {
    ensure!(max_amount > 0, "surplus amount must be positive");
    validate_request(params, source, destination, 1, secret)?;
    fee_for_mass(0, feerate)?;
    let max_inputs = MAX_TRANSFER_INPUTS.min(params.max_tx_inputs);
    ensure!(max_inputs > 0, "transfer input limit must be positive");

    let source_script = pay_to_address_script(source);
    let mut eligible = utxos
        .into_iter()
        .filter(|(_, entry)| {
            entry.script_public_key == source_script
                && entry.amount > 0
                && entry.covenant_id.is_none()
                && (!entry.is_coinbase
                    || entry.block_daa_score.checked_add(params.coinbase_maturity()).is_some_and(|mature| mature <= daa_score))
        })
        .collect::<Vec<_>>();
    eligible.sort_by(|left, right| right.1.amount.cmp(&left.1.amount));
    ensure!(!eligible.is_empty(), "InsufficientFunds: no eligible surplus UTXOs");

    let mut prefix_totals = Vec::with_capacity(eligible.len());
    let mut total = 0u64;
    for (_, entry) in &eligible {
        total = total.checked_add(entry.amount).context("surplus input total overflows")?;
        prefix_totals.push(total);
    }

    let mut batch_len = eligible.len().min(max_inputs);
    let mut last_mass_error = None;
    while batch_len > 0 {
        let batch = &eligible[..batch_len];
        let batch_total = prefix_totals[batch_len - 1];
        let conservative_fee = surplus_fee_upper_bound(params, source, destination, feerate, batch)?;
        let amount = max_amount.min(batch_total.saturating_sub(conservative_fee));
        if amount == 0 {
            bail!("InsufficientFunds: eligible surplus does not cover its transaction fee");
        }
        match build_transfer(params, daa_score, source, destination, amount, None, feerate, secret, batch.to_vec()) {
            Ok(signed) => return Ok(signed),
            Err(error) if is_mass_limit_error(&error) => {
                last_mass_error = Some(error);
                if batch_len == 1 {
                    break;
                }
                batch_len = (batch_len / 2).max(1);
            }
            Err(error) => return Err(error),
        }
    }
    if let Some(error) = last_mass_error {
        return Err(error.context("surplus transfer cannot fit the mass limits"));
    }
    bail!("InsufficientFunds: no eligible surplus batch can fund a transfer")
}

fn surplus_fee_upper_bound(
    params: &Params,
    source: &Address,
    destination: &Address,
    feerate: f64,
    batch: &[(TransactionOutpoint, UtxoEntry)],
) -> Result<u64> {
    let tx = Transaction::new(
        TX_VERSION,
        batch.iter().map(|(outpoint, _)| TransactionInput::new(*outpoint, vec![0; 66], MAX_TX_IN_SEQUENCE_NUM, 1)).collect(),
        vec![TransactionOutput::new(1, pay_to_address_script(destination)), TransactionOutput::new(1, pay_to_address_script(source))],
        0,
        SUBNETWORK_ID_NATIVE,
        0,
        vec![],
    );
    let mass = MassCalculator::new_with_consensus_params(params)
        .calc_non_contextual_masses(&tx)
        .normalized_max(&params.block_mass_cofactors());
    fee_for_mass(mass, feerate)
}

fn is_mass_limit_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("exceeds non-contextual mass limits") || message.contains("SmallTransferNotSupported")
}

pub(crate) fn validate_request(
    params: &Params,
    source: &Address,
    destination: &Address,
    amount: u64,
    secret: &[u8; 32],
) -> Result<()> {
    ensure!(amount > 0, "transfer amount must be positive");
    ensure!(amount <= celenium_consensus_core::constants::MAX_PRADHA, "transfer amount exceeds the monetary limit");
    ensure!(source.prefix == params.prefix() && destination.prefix == params.prefix(), "address network does not match node network");
    ensure!(source.version == Version::PubKey, "source must be a Schnorr public-key address");
    let pair = secp256k1::Keypair::from_seckey_slice(secp256k1::SECP256K1, secret).context("invalid secret key")?;
    let expected = Address::new(params.prefix(), Version::PubKey, &pair.x_only_public_key().0.serialize());
    ensure!(source == &expected, "secret key does not match source address");
    Ok(())
}

pub(crate) fn fee_for_mass(mass: u64, rate: f64) -> Result<u64> {
    ensure!(rate.is_finite() && rate > 0.0, "invalid fee rate");
    let fee = (mass as f64 * rate).ceil();
    ensure!(fee.is_finite() && fee < u64::MAX as f64, "fee overflows");
    Ok(fee as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use celenium_addresses::{Prefix, Version};
    use celenium_consensus_core::{config::params::SIMNET_PARAMS, mass::MassCalculator};
    use celenium_txscript::pay_to_address_script;
    use secp256k1::{Keypair, SECP256K1};

    fn wallet(byte: u8) -> (Address, [u8; 32]) {
        let secret = [byte; 32];
        let key = Keypair::from_seckey_slice(SECP256K1, &secret).unwrap();
        (Address::new(Prefix::Simnet, Version::PubKey, &key.x_only_public_key().0.serialize()), secret)
    }

    fn inputs(source: &Address, amounts: &[u64]) -> Vec<(TransactionOutpoint, UtxoEntry)> {
        amounts
            .iter()
            .enumerate()
            .map(|(i, amount)| {
                (
                    TransactionOutpoint::new(celenium_hashes::Hash::from_u64_word(i as u64 + 1), 0),
                    UtxoEntry::new(*amount, pay_to_address_script(source), 1, true, None),
                )
            })
            .collect()
    }

    #[test]
    fn surplus_batches_bootstrap_inputs_and_can_sweep_again() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut remaining = inputs(&source, &[2_079_547; 200]);
        let mut maximum = 400_000_000;
        for _ in 0..2 {
            let signed = build_surplus_transfer(&SIMNET_PARAMS, 1, &source, &dest, maximum, 0.25, &secret, remaining.clone()).unwrap();
            assert!(!signed.tx.inputs.is_empty());
            assert!(signed.tx.inputs.len() <= MAX_TRANSFER_INPUTS.min(SIMNET_PARAMS.max_tx_inputs));
            let amount = signed.tx.outputs[0].value;
            let total: u64 = signed.entries.iter().map(|entry| entry.as_ref().unwrap().amount).sum();
            let paid = total - signed.tx.outputs.iter().map(|output| output.value).sum::<u64>();
            let calculator = MassCalculator::new_with_consensus_params(&SIMNET_PARAMS);
            let mass = calculator.calc_non_contextual_masses(&signed.tx);
            assert!(amount > 0 && amount <= maximum && amount < total);
            assert!(paid >= fee_for_mass(mass.normalized_max(&SIMNET_PARAMS.block_mass_cofactors()), 0.25).unwrap());
            assert!(mass.compute_mass <= SIMNET_PARAMS.block_mass_limits.compute);
            assert!(mass.transient_mass <= SIMNET_PARAMS.block_mass_limits.transient);
            assert!(
                calculator.calc_contextual_masses(&signed.as_verifiable()).unwrap().storage_mass
                    <= SIMNET_PARAMS.block_mass_limits.storage
            );
            sign::verify(&signed.as_verifiable()).unwrap();
            remaining.retain(|(outpoint, _)| !signed.tx.inputs.iter().any(|input| input.previous_outpoint == *outpoint));
            for (index, output) in signed.tx.outputs.iter().enumerate().skip(1) {
                remaining.push((
                    TransactionOutpoint::new(signed.tx.id(), index as u32),
                    UtxoEntry::new(output.value, output.script_public_key.clone(), 1, false, None),
                ));
            }
            let quote = PreparedTransfer::new(signed, &SIMNET_PARAMS).unwrap();
            assert!(!quote.input_entries().is_empty());
            maximum -= amount;
        }
    }

    #[test]
    fn surplus_prefers_large_change_and_respects_network_input_limit() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut params = SIMNET_PARAMS;
        params.max_tx_inputs = 1;
        let mut utxos = inputs(&source, &[2_079_547; 200]);
        let change = TransactionOutpoint::new(celenium_hashes::Hash::from_u64_word(201), 0);
        utxos.push((change, UtxoEntry::new(300_000_000, pay_to_address_script(&source), 1, false, None)));
        let signed = build_surplus_transfer(&params, 1, &source, &dest, 100_000_000, 0.25, &secret, utxos).unwrap();
        assert_eq!(signed.tx.inputs.len(), 1);
        assert_eq!(signed.tx.inputs[0].previous_outpoint, change);
        assert_eq!(signed.tx.outputs[0].value, 100_000_000);
        assert_eq!(signed.tx.outputs[1].script_public_key, pay_to_address_script(&source));
    }

    #[test]
    fn surplus_reduces_batches_that_exceed_compute_mass() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut params = SIMNET_PARAMS;
        params.block_mass_limits.compute = 5_000;
        let signed =
            build_surplus_transfer(&params, 1, &source, &dest, 400_000_000, 0.25, &secret, inputs(&source, &[2_079_547; 200]))
                .unwrap();
        assert!(signed.tx.inputs.len() < MAX_TRANSFER_INPUTS);
        assert!(
            MassCalculator::new_with_consensus_params(&params).calc_non_contextual_masses(&signed.tx).compute_mass
                <= params.block_mass_limits.compute
        );
    }

    #[test]
    fn surplus_rejects_unspendable_inputs_and_invalid_limits_without_panicking() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut params = SIMNET_PARAMS;
        params.blockrate.coinbase_maturity = 10;
        let mut utxos = inputs(&source, &[200_000_000; 4]);
        utxos[1].1.block_daa_score = u64::MAX;
        utxos[2].1.covenant_id = Some(Default::default());
        utxos[3].1.script_public_key = pay_to_address_script(&dest);
        assert!(build_surplus_transfer(&params, 10, &source, &dest, 100_000_000, 0.25, &secret, utxos).is_err());
        for (maximum, rate, amounts) in [
            (0, 0.25, vec![200_000_000]),
            (100_000_000, f64::NAN, vec![200_000_000]),
            (100_000_000, 0.25, vec![1]),
            (100_000_000, 0.25, vec![]),
            (100_000_000, f64::MAX, vec![200_000_000]),
            (100_000_000, 0.25, vec![u64::MAX; 2]),
        ] {
            assert!(
                build_surplus_transfer(&SIMNET_PARAMS, 1, &source, &dest, maximum, rate, &secret, inputs(&source, &amounts)).is_err()
            );
        }
        params.max_tx_inputs = 0;
        assert!(
            build_surplus_transfer(&params, 11, &source, &dest, 100_000_000, 0.25, &secret, inputs(&source, &[200_000_000])).is_err()
        );
    }

    #[test]
    fn exact_input_value_omits_zero_change_and_keeps_requested_fee() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let signed =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, Some(509), 0.25, &secret, inputs(&source, &[100_000_509]))
                .unwrap();
        assert_eq!(signed.tx.outputs.len(), 1);
        assert_eq!(signed.tx.outputs[0].value, 100_000_000);
        let quote = PreparedTransfer::new(signed, &SIMNET_PARAMS).unwrap();
        assert_eq!(quote.fee_pradha(), 509);
    }

    #[test]
    fn maturity_comes_from_network_parameters() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut params = SIMNET_PARAMS;
        params.blockrate.coinbase_maturity = 10;
        assert!(
            build_transfer(&params, 10, &source, &dest, 100_000_000, None, 0.25, &secret, inputs(&source, &[200_000_000])).is_err()
        );
        assert!(
            build_transfer(&params, 11, &source, &dest, 100_000_000, None, 0.25, &secret, inputs(&source, &[200_000_000])).is_ok()
        );
    }

    #[test]
    fn opt_in_simnet_quote_preserves_small_amounts_and_charges_real_fee() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let mut params = SIMNET_PARAMS;
        params.storage_mass_amount_floor = 4_000_000;
        for amount in [1, 1000] {
            let signed =
                build_transfer(&params, 1, &source, &dest, amount, None, 0.25, &secret, inputs(&source, &[2_079_547])).unwrap();
            assert_eq!(signed.tx.outputs[0].value, amount);
            assert_eq!(signed.tx.outputs.len(), 2);
            assert_eq!(
                MassCalculator::new_with_consensus_params(&params)
                    .calc_contextual_masses(&signed.as_verifiable())
                    .unwrap()
                    .storage_mass,
                250_000
            );
            let quote = PreparedTransfer::new(signed, &params).unwrap();
            assert_eq!(quote.fee_pradha(), 509);
        }
    }

    #[test]
    fn zero_amount_is_rejected_before_mass_calculation() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let error = build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 0, Some(509), 0.25, &secret, inputs(&source, &[200_000_000]))
            .unwrap_err();
        assert!(error.to_string().contains("positive"));
    }

    #[test]
    fn validates_network_and_source_key_before_selection() {
        let (source, secret) = wallet(1);
        let (mut dest, _) = wallet(2);
        dest.prefix = Prefix::Mainnet;
        let error =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, Some(509), 0.25, &secret, inputs(&source, &[200_000_000]))
                .unwrap_err();
        assert!(error.to_string().contains("network"));
        dest.prefix = Prefix::Simnet;
        let error =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, Some(509), 0.25, &[3; 32], inputs(&source, &[200_000_000]))
                .unwrap_err();
        assert!(error.to_string().contains("source"));
        let error =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, Some(509), 0.25, &[0; 32], inputs(&source, &[200_000_000]))
                .unwrap_err();
        assert!(error.to_string().contains("secret"));
    }

    #[test]
    fn tiny_legacy_transfer_returns_explicit_error() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let error = build_transfer(
            &SIMNET_PARAMS,
            1,
            &source,
            &dest,
            1_000,
            None,
            0.25,
            &secret,
            inputs(&source, &[2_079_547; MAX_TRANSFER_INPUTS]),
        )
        .unwrap_err();
        assert!(error.to_string().contains("SmallTransferNotSupported"));
    }

    #[test]
    fn exact_fee_rejects_underpayment_instead_of_changing_it() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let result =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, Some(275), 0.25, &secret, inputs(&source, &[200_000_000]));
        assert!(result.is_err(), "an underpaid fee must not produce a transaction");
        assert!(result.unwrap_err().to_string().contains("required 509"));
    }

    #[test]
    fn quote_uses_signed_mass_and_ceil_fee() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let signed =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 100_000_000, None, 0.25, &secret, inputs(&source, &[200_000_000]))
                .unwrap();
        let calculator = MassCalculator::new_with_consensus_params(&SIMNET_PARAMS);
        assert_eq!(calculator.calc_non_contextual_masses(&signed.tx).compute_mass, 2036);
        assert_eq!(200_000_000 - signed.tx.outputs.iter().map(|o| o.value).sum::<u64>(), 509);
    }

    #[test]
    fn adds_inputs_until_storage_mass_fits() {
        let (source, secret) = wallet(1);
        let (dest, _) = wallet(2);
        let signed =
            build_transfer(&SIMNET_PARAMS, 1, &source, &dest, 1_039_773, Some(2_000), 0.25, &secret, inputs(&source, &[2_079_547; 4]))
                .unwrap();
        let calculator = MassCalculator::new_with_consensus_params(&SIMNET_PARAMS);
        assert!(signed.tx.inputs.len() > 1);
        assert!(
            calculator.calc_contextual_masses(&signed.as_verifiable()).unwrap().storage_mass
                <= SIMNET_PARAMS.block_mass_limits.storage
        );
        assert_eq!(signed.tx.outputs[0].value, 1_039_773);
        celenium_consensus_core::sign::verify(&signed.as_verifiable()).unwrap();
    }
}
