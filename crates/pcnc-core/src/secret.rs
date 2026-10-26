//! Sealing of credentials at rest and generation of random passwords.

use anyhow::{Context, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::{Rng, RngCore, seq::SliceRandom};

/// Encrypts registry fields with ChaCha20-Poly1305. The associated data binds each value to
/// its identity and field, so a sealed value cannot be moved to another row or column.
#[derive(Clone)]
pub struct Sealer {
    cipher: ChaCha20Poly1305,
}

impl Sealer {
    /// `key_b64` is 32 random bytes, base64-encoded (`openssl rand -base64 32`).
    pub fn from_base64(key_b64: &str) -> anyhow::Result<Self> {
        let key = B64
            .decode(key_b64.trim())
            .context("state key is not valid base64")?;
        if key.len() != 32 {
            bail!("state key must decode to 32 bytes, got {}", key.len());
        }
        Ok(Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
        })
    }

    pub fn seal(&self, aad: &str, plaintext: &str) -> anyhow::Result<String> {
        let mut nonce = [0u8; 12];
        rand::rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("sealing failed"))?;
        let mut out = nonce.to_vec();
        out.extend(ct);
        Ok(B64.encode(out))
    }

    pub fn open(&self, aad: &str, sealed: &str) -> anyhow::Result<String> {
        let raw = B64
            .decode(sealed)
            .context("sealed value is not valid base64")?;
        if raw.len() < 12 {
            bail!("sealed value too short");
        }
        let (nonce, ct) = raw.split_at(12);
        let pt = self
            .cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("sealed value cannot be opened with this state key"))?;
        String::from_utf8(pt).context("sealed value is not UTF-8")
    }
}

const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const DIGIT: &[u8] = b"23456789";
const SPECIAL: &[u8] = b"-_.";

/// A 32-character password with at least one lowercase letter, uppercase letter, digit and
/// one of `-_.`, so it satisfies common password policies and needs no escaping anywhere.
pub fn generate_password() -> String {
    let mut rng = rand::rng();
    let all: Vec<u8> = [LOWER, UPPER, DIGIT].concat();
    let mut chars: Vec<u8> = vec![
        LOWER[rng.random_range(0..LOWER.len())],
        UPPER[rng.random_range(0..UPPER.len())],
        DIGIT[rng.random_range(0..DIGIT.len())],
        SPECIAL[rng.random_range(0..SPECIAL.len())],
    ];
    while chars.len() < 32 {
        chars.push(all[rng.random_range(0..all.len())]);
    }
    chars.shuffle(&mut rng);
    String::from_utf8(chars).expect("ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealer() -> Sealer {
        Sealer::from_base64(&B64.encode([7u8; 32])).unwrap()
    }

    #[test]
    fn roundtrip_and_binding() {
        let s = sealer();
        let sealed = s.seal("agent-1/mcp", "hunter2").unwrap();
        assert_eq!(s.open("agent-1/mcp", &sealed).unwrap(), "hunter2");
        assert!(s.open("agent-2/mcp", &sealed).is_err());
    }

    #[test]
    fn rejects_short_keys() {
        assert!(Sealer::from_base64(&B64.encode([1u8; 16])).is_err());
    }

    #[test]
    fn passwords_have_all_classes() {
        for _ in 0..100 {
            let p = generate_password();
            assert_eq!(p.len(), 32);
            assert!(p.bytes().any(|b| LOWER.contains(&b)));
            assert!(p.bytes().any(|b| UPPER.contains(&b)));
            assert!(p.bytes().any(|b| DIGIT.contains(&b)));
            assert!(p.bytes().any(|b| SPECIAL.contains(&b)));
        }
    }
}
