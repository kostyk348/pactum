//! Приёмник нотариуса: подписанное заявление «этот документ имел этот хеш».
//!
//! Каноническая строка собирается из полей приёмника и хешируется SHA-256 —
//! так же, как блоки в `pactum-core::facts`. Подпись ставится на хеш записи,
//! а сам приёмник несёт открытый ключ нотариуса: файл приёмника проверяется
//! офлайн, без сервера и без сети.

use crate::error::{WitnessError, WitnessResult};
use pactum_core::hash;
use pactum_identity::{self, KeyPair};
use serde::{Deserialize, Serialize};

/// Версия формата приёмника.
pub const RECEIPT_V: u32 = 1;

/// Подписанный приёмник нотариуса.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    /// Версия формата (`1`).
    pub v: u32,
    /// Кто нотариовал: `p` + 16 hex (из открытого ключа).
    pub witness: String,
    /// Открытый ключ нотариуса (130 hex, несжатый SEC1).
    pub pubkey: String,
    /// Номер записи в журнале, начиная с 1.
    pub seq: u64,
    /// Момент нотаризации, ISO-8601 UTC.
    pub ts: String,
    /// SHA-256 содержимого, 64 hex.
    pub content_sha256: String,
    /// Размер содержимого в байтах.
    pub size: u64,
    /// Заявленный media type (из заголовка или параметра), если был.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Заметка: url, имя файла, пояснение.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Хранится ли само содержимое рядом с журналом (blobs/).
    pub stored: bool,
    /// Хеш предыдущей записи (пусто у генезиса).
    pub prev_hash: String,
    /// Хеш этой записи (каноническая строка → SHA-256).
    pub entry_hash: String,
    /// Подпись нотариуса на `entry_hash` (`ecdsa-p256:<hex>`).
    pub sig: String,
}

impl Receipt {
    /// Каноническая строка: всё, что подписывается. `-` для отсутствующих
    /// полей — чтобы разделители не двусмыслили.
    pub fn canonical(&self) -> String {
        format!(
            "witness/{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            self.v,
            self.witness,
            self.seq,
            self.ts,
            self.content_sha256,
            self.size,
            self.media_type.as_deref().unwrap_or("-"),
            self.note.as_deref().unwrap_or("-"),
            if self.stored { "1" } else { "0" },
            self.prev_hash
        )
    }

    /// Хеш записи: SHA-256 канонической строки.
    pub fn entry_hash(&self) -> String {
        hash::sha256_text(&self.canonical())
    }

    /// Дозаполнить вычисляемые поля: `entry_hash` и подпись.
    pub fn seal(&mut self, key: &KeyPair) {
        self.witness = key.node_id();
        self.pubkey = key.pubkey_hex();
        self.entry_hash = self.entry_hash();
        self.sig = key.sign(self.entry_hash.as_bytes());
    }
}

/// Проверить приёмник целиком: хеш, форма дайджеста, идентичность и подпись.
/// Не требует сети: открытый ключ лежит в самом приёмнике.
pub fn verify_receipt(receipt: &Receipt) -> WitnessResult<()> {
    let recomputed = receipt.entry_hash();
    if recomputed != receipt.entry_hash {
        return Err(WitnessError::Hash(format!(
            "заявлен {}, вычислен {}",
            receipt.entry_hash, recomputed
        )));
    }
    if receipt.v != RECEIPT_V {
        return Err(WitnessError::Hash(format!(
            "неизвестная версия приёмника: {}",
            receipt.v
        )));
    }
    if !is_sha256_hex(&receipt.content_sha256) {
        return Err(WitnessError::Hash(
            "content_sha256 не 64 hex-символа".into(),
        ));
    }
    let pk_bytes = hex::decode(&receipt.pubkey)
        .map_err(|err| WitnessError::Identity(format!("публичный ключ не hex: {err}")))?;
    let expected_id = pactum_core::id::node_id_from_pubkey(&pk_bytes);
    if expected_id != receipt.witness {
        return Err(WitnessError::Identity(format!(
            "witness {} не соответствует ключу ({expected_id})",
            receipt.witness
        )));
    }
    pactum_identity::verify(&receipt.pubkey, receipt.entry_hash.as_bytes(), &receipt.sig)
        .map_err(|err| WitnessError::Signature(err.to_string()))?;
    Ok(())
}

/// Проверить приёмник и потребовать, чтобы подписал его ожидаемый нотариус.
pub fn verify_receipt_pinned(receipt: &Receipt, expected_pubkey: &str) -> WitnessResult<()> {
    if receipt.pubkey != expected_pubkey {
        return Err(WitnessError::Identity(
            "приёмник подписан чужим ключом (pin не сошёлся)".into(),
        ));
    }
    verify_receipt(receipt)
}

fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) && text == text.to_lowercase()
}

/// Подписанная голова журнала — checkpoint для со-свидетельствования.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChainHead {
    /// Номер последней записи.
    pub seq: u64,
    /// Хеш последней записи.
    pub head_hash: String,
    /// Момент постановки подписи, ISO-8601 UTC.
    pub ts: String,
    /// Подпись нотариуса на канонической строке головы.
    pub sig: String,
}

impl ChainHead {
    pub fn canonical(&self) -> String {
        format!("witness-head/1|{}|{}|{}", self.seq, self.head_hash, self.ts)
    }

    pub fn sign(&mut self, key: &KeyPair) {
        self.sig = key.sign(self.canonical().as_bytes());
    }

    pub fn verify(&self, pubkey: &str) -> WitnessResult<()> {
        pactum_identity::verify(pubkey, self.canonical().as_bytes(), &self.sig)
            .map_err(|err| WitnessError::Signature(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sealed(text: &str) -> Receipt {
        let key = KeyPair::generate();
        let mut receipt = Receipt {
            v: RECEIPT_V,
            witness: String::new(),
            pubkey: String::new(),
            seq: 1,
            ts: pactum_core::time::now_iso(),
            content_sha256: hash::sha256_text(text),
            size: text.len() as u64,
            media_type: Some("text/plain".into()),
            note: Some("тест".into()),
            stored: false,
            prev_hash: String::new(),
            entry_hash: String::new(),
            sig: String::new(),
        };
        receipt.seal(&key);
        receipt
    }

    #[test]
    fn receipt_seal_and_verify() {
        let receipt = sealed("документ дня");
        assert_eq!(receipt.entry_hash.len(), 64);
        assert!(verify_receipt(&receipt).is_ok());
    }

    #[test]
    fn tampered_field_breaks_receipt() {
        let mut receipt = sealed("документ дня");
        receipt.note = Some("переписали".into());
        assert!(matches!(
            verify_receipt(&receipt),
            Err(WitnessError::Hash(_))
        ));
        let mut receipt = sealed("документ дня");
        receipt.content_sha256 = "0".repeat(64);
        assert!(verify_receipt(&receipt).is_err());
    }

    #[test]
    fn foreign_key_breaks_identity() {
        let mut receipt = sealed("документ дня");
        let other = KeyPair::generate();
        receipt.pubkey = other.pubkey_hex();
        assert!(matches!(
            verify_receipt(&receipt),
            Err(WitnessError::Identity(_))
        ));
    }

    #[test]
    fn pinned_key_mismatch_detected() {
        let receipt = sealed("документ дня");
        let stranger = KeyPair::generate();
        assert!(verify_receipt_pinned(&receipt, &stranger.pubkey_hex()).is_err());
        assert!(verify_receipt_pinned(&receipt, &receipt.pubkey).is_ok());
    }

    #[test]
    fn head_sign_and_verify() {
        let key = KeyPair::generate();
        let mut head = ChainHead {
            seq: 3,
            head_hash: "a".repeat(64),
            ts: pactum_core::time::now_iso(),
            sig: String::new(),
        };
        head.sign(&key);
        assert!(head.verify(&key.pubkey_hex()).is_ok());
        assert!(head.verify(&KeyPair::generate().pubkey_hex()).is_err());
    }

    #[test]
    fn canonical_is_stable() {
        // канонизация и подпись детерминированы: те же поля и тот же ключ
        // дают байт-в-байт тот же приёмник
        let key = KeyPair::from_secret_bytes(&[7u8; 32]).unwrap();
        let mut a = sealed("x");
        a.seal(&key);
        let mut b = a.clone();
        b.entry_hash = String::new();
        b.sig = String::new();
        b.seal(&key);
        assert_eq!(a, b);
    }
}
