mod utxo_set_override_inner {
    use std::sync::Arc;

    use celenium_consensus_core::{
        api::ConsensusApi,
        config::{Config, genesis::GenesisBlock},
        header::Header,
        muhash::MuHashExtensions,
        tx::TransactionOutpoint,
        utxo::UtxoEntry,
    };
    use celenium_hashes::Hash;
    use celenium_muhash::MuHash;

    use crate::consensus::Consensus;

    /// Import genesis outputs as non-coinbase entries; CLTV premine scripts still enforce vesting.
    pub fn genesis_premine_utxos(genesis: &GenesisBlock) -> Vec<(TransactionOutpoint, UtxoEntry)> {
        let mut utxos = Vec::new();
        for tx in genesis.build_genesis_transactions() {
            let txid = tx.id();
            for (index, out) in tx.outputs.iter().enumerate() {
                utxos.push((
                    TransactionOutpoint { transaction_id: txid, index: index as u32 },
                    UtxoEntry {
                        amount: out.value,
                        script_public_key: out.script_public_key.clone(),
                        block_daa_score: 0,
                        is_coinbase: false,
                        covenant_id: None,
                    },
                ));
            }
        }
        utxos
    }

    pub fn set_genesis_utxo_commitment_from_config(config: &mut Config) {
        let mut genesis_multiset = MuHash::new();
        for (outpoint, entry) in genesis_premine_utxos(&config.params.genesis) {
            genesis_multiset.add_utxo(&outpoint, &entry);
        }
        for (outpoint, entry) in config.initial_utxo_set.iter() {
            genesis_multiset.add_utxo(outpoint, entry);
        }

        config.params.genesis.utxo_commitment = genesis_multiset.finalize();
        let genesis_header: Header = (&config.params.genesis).into();
        config.params.genesis.hash = genesis_header.hash;
    }

    pub fn set_initial_utxo_set(config: &Config, consensus: Arc<Consensus>, genesis_hash: Hash) {
        let mut utxos = genesis_premine_utxos(&config.params.genesis);
        utxos.extend(config.initial_utxo_set.iter().map(|(op, entry)| (*op, entry.clone())));
        let utxo_slice = &utxos[..];
        let mut genesis_multiset = MuHash::new();
        consensus.append_imported_pruning_point_utxos(utxo_slice, &mut genesis_multiset);
        consensus.import_pruning_point_utxo_set(genesis_hash, genesis_multiset).unwrap();
    }
}

pub use utxo_set_override_inner::*;
