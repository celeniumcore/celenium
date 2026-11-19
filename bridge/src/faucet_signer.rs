//! Signer wallet faucet (operator-side).
//!
//! Kunci privat faucet hanya dipakai bridge untuk menandatangani transaksi
//! penarikan on-chain. Kunci dibaca dari environment (bukan config.yaml, bukan
//! database) supaya tidak ikut ter-backup/ter-commit bersama konfigurasi:
//!
//! - `CELENIUM_FAUCET_PRIVKEY` — hex 32 byte
//! - `CELENIUM_FAUCET_KEYFILE` — path ke file berisi `privkey=<hex>` (format
//!   yang ditulis `gen_faucet_key`)
//!
//! Saat startup alamat diturunkan ulang dari kunci dan dibandingkan dengan
//! `faucet_address` di config. Kalau beda, bridge menolak membayar — lebih baik
//! gagal keras daripada mengirim dari alamat yang bukan milik kunci ini.

use celenium_addresses::{Address, Prefix, Version};
use secp256k1::{Keypair, Secp256k1};

/// Kunci privat faucet, dibaca sekali saat startup.
#[derive(Clone)]
pub struct FaucetSigner {
    secret_hex: String,
    address: String,
}

impl std::fmt::Debug for FaucetSigner {
    /// Jangan pernah cetak kunci privat ke log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaucetSigner").field("address", &self.address).finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum SignerError {
    NoKeyConfigured,
    BadKeyFile(String),
    BadSecret(String),
    AddressMismatch { derived: String, configured: String },
}

impl std::fmt::Display for SignerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignerError::NoKeyConfigured => {
                write!(f, "faucet signer key not configured (set CELENIUM_FAUCET_PRIVKEY or CELENIUM_FAUCET_KEYFILE)")
            }
            SignerError::BadKeyFile(e) => write!(f, "cannot read faucet key file: {e}"),
            SignerError::BadSecret(e) => write!(f, "invalid faucet private key: {e}"),
            SignerError::AddressMismatch { derived, configured } => {
                write!(f, "faucet key derives address {derived} but config faucet_address is {configured}")
            }
        }
    }
}

impl std::error::Error for SignerError {}

impl FaucetSigner {
    /// Baca kunci dari environment lalu turunkan alamatnya.
    pub fn from_env() -> Result<Self, SignerError> {
        Self::from_env_for_network(Prefix::Mainnet)
    }

    pub fn from_env_for_network(prefix: Prefix) -> Result<Self, SignerError> {
        let secret_hex = match std::env::var("CELENIUM_FAUCET_PRIVKEY") {
            Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => {
                let path = std::env::var("CELENIUM_FAUCET_KEYFILE").map_err(|_| SignerError::NoKeyConfigured)?;
                return Self::from_keyfile_for_network(std::path::Path::new(&path), prefix);
            }
        };
        Self::from_secret_hex_for_network(secret_hex, prefix)
    }

    pub fn from_reserve_env(prefix: Prefix) -> Result<Self, SignerError> {
        let path = std::env::var_os("CELENIUM_FAUCET_RESERVE_KEYFILE")
            .ok_or_else(|| SignerError::BadKeyFile("set CELENIUM_FAUCET_RESERVE_KEYFILE".into()))?;
        Self::from_keyfile_for_network(std::path::Path::new(&path), prefix)
    }

    pub fn from_keyfile_for_network(path: &std::path::Path, prefix: Prefix) -> Result<Self, SignerError> {
        let text = std::fs::read_to_string(path).map_err(|e| SignerError::BadKeyFile(e.to_string()))?;
        let secret_hex = text
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == "privkey" && !value.trim().is_empty()).then(|| value.trim().to_string())
            })
            .ok_or_else(|| SignerError::BadKeyFile("no `privkey=` line".into()))?;
        Self::from_secret_hex_for_network(secret_hex, prefix)
    }

    pub fn from_secret_hex(secret_hex: String) -> Result<Self, SignerError> {
        Self::from_secret_hex_for_network(secret_hex, Prefix::Mainnet)
    }

    pub fn from_secret_hex_for_network(secret_hex: String, prefix: Prefix) -> Result<Self, SignerError> {
        let secret = hex_bytes(&secret_hex).map_err(SignerError::BadSecret)?;
        let secret: [u8; 32] =
            secret.try_into().map_err(|v: Vec<u8>| SignerError::BadSecret(format!("expected 32 bytes, got {}", v.len())))?;

        let secp = Secp256k1::new();
        let keypair = Keypair::from_seckey_slice(&secp, &secret)
            .map_err(|e| SignerError::BadSecret(format!("not a valid secp256k1 secret: {e}")))?;
        let (xonly, _parity) = keypair.x_only_public_key();
        let address = Address::new(prefix, Version::PubKey, &xonly.serialize()).to_string();

        Ok(Self { secret_hex: hex_string(&secret), address })
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    /// Hex kunci privat, untuk diteruskan ke `send_coins`. Hanya dipakai di
    /// dalam proses; tidak pernah diserialisasi atau dicetak.
    pub fn secret_hex(&self) -> &str {
        &self.secret_hex
    }

    /// Tolak penandatanganan bila alamat config bukan alamat kunci ini.
    pub fn ensure_matches(&self, configured: &str) -> Result<(), SignerError> {
        if configured == self.address {
            Ok(())
        } else {
            Err(SignerError::AddressMismatch { derived: self.address.clone(), configured: configured.to_string() })
        }
    }
}

fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim().trim_start_matches("0x");
    if !s.len().is_multiple_of(2) {
        return Err("odd number of hex digits".to_string());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for i in (0..bytes.len()).step_by(2) {
        let pair = std::str::from_utf8(&bytes[i..i + 2]).map_err(|_| "non-utf8 input".to_string())?;
        out.push(u8::from_str_radix(pair, 16).map_err(|_| "input contains non-hex digits".to_string())?);
    }
    Ok(out)
}

fn hex_string(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    // Kunci uji tetap: hanya untuk membuktikan derivasi alamat deterministik.
    const TEST_SECRET: &str = "50fb2205d2f302ff1dc8554f24e185ed6a32daba6b7a73bc89e31014439004e7";

    #[test]
    fn network_aware_signer_retains_mainnet_default_and_redacts_secret() {
        let main = FaucetSigner::from_secret_hex(TEST_SECRET.into()).unwrap();
        let sim = FaucetSigner::from_secret_hex_for_network(TEST_SECRET.into(), Prefix::Simnet).unwrap();
        let parsed = Address::try_from(sim.address()).unwrap();
        assert_eq!(parsed.prefix, Prefix::Simnet);
        assert_ne!(main.address(), sim.address());
        assert!(main.ensure_matches(sim.address()).is_err());
        assert!(sim.ensure_matches(&format!(" {}", sim.address())).is_err());
        assert!(!format!("{sim:?}").contains(TEST_SECRET));
    }

    #[test]
    fn derives_stable_mainnet_address() {
        let a = FaucetSigner::from_secret_hex(TEST_SECRET.to_string()).unwrap();
        let b = FaucetSigner::from_secret_hex(TEST_SECRET.to_string()).unwrap();
        assert_eq!(a.address(), b.address());
        assert!(a.address().starts_with("cen69:"));
    }

    #[test]
    fn ensure_matches_rejects_other_address() {
        let s = FaucetSigner::from_secret_hex(TEST_SECRET.to_string()).unwrap();
        assert!(s.ensure_matches(s.address()).is_ok());
        assert!(s.ensure_matches("cen69:qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqem807kn3").is_err());
    }

    #[test]
    fn rejects_malformed_secret() {
        assert!(FaucetSigner::from_secret_hex("zz".to_string()).is_err());
        assert!(FaucetSigner::from_secret_hex("ab".to_string()).is_err());
    }

    #[test]
    fn debug_never_prints_secret() {
        let s = FaucetSigner::from_secret_hex(TEST_SECRET.to_string()).unwrap();
        let rendered = format!("{s:?}");
        assert!(!rendered.contains(TEST_SECRET));
        assert!(rendered.contains(s.address()));
    }
}
