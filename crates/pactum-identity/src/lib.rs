//! # pactum-identity
//!
//! Идентичность узла Pactum: пара ECDSA P-256 (как в acp-mvp — SECP256R1),
//! зашифрованное хранилище ключа и подпись писем.
//!
//! Участник протокола подписывает всё, что отправляет. Открытый ключ — это и
//! есть паспорт узла: из него выводится короткий `node id`. Ключ шифруется
//! AES-256-GCM с ключом из PBKDF2-HMAC-SHA256 (200 000 итераций).

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, OsRng as AeadOsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use thiserror::Error;

/// Итерации PBKDF2. Дорогие намеренно: хранилище вскрывают офлайн.
pub const PBKDF2_ROUNDS: u32 = 200_000;

#[derive(Debug, Error)]
pub enum IdentityError {
    #[error("ключ не разобран: {0}")]
    Key(String),
    #[error("подпись не сошлась")]
    BadSignature,
    #[error("хранилище: {0}")]
    Keystore(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type IdentityResult<T> = Result<T, IdentityError>;

/// Ключевая пара узла.
#[derive(Clone)]
pub struct KeyPair {
    signing: SigningKey,
}

impl KeyPair {
    /// Сгенерировать новую пару (ос случайность).
    pub fn generate() -> KeyPair {
        KeyPair {
            signing: SigningKey::random(&mut rand::rngs::OsRng),
        }
    }

    /// Восстановить из 32-байтного секрета.
    pub fn from_secret_bytes(bytes: &[u8]) -> IdentityResult<KeyPair> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| IdentityError::Key("секрет должен быть 32 байта".into()))?;
        let signing = SigningKey::from_bytes(&arr.into())
            .map_err(|err| IdentityError::Key(format!("{err}")))?;
        Ok(KeyPair { signing })
    }

    /// 32 байта секрета (для шифрования в keystore).
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes().into()
    }

    /// Открытый ключ в несжатом SEC1 (65 байт → 130 hex), как в acp-mvp.
    pub fn pubkey_hex(&self) -> String {
        hex::encode(
            self.signing
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes(),
        )
    }

    /// Короткий node id: `p` + 16 hex от sha256 открытого ключа.
    pub fn node_id(&self) -> String {
        pactum_core::id::node_id_from_pubkey(&self.pubkey_hex_bytes())
    }

    fn pubkey_hex_bytes(&self) -> Vec<u8> {
        self.signing
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec()
    }

    /// Подписать байты. Возвращает `ecdsa-p256:<hex>` (64 байта r||s).
    pub fn sign(&self, data: &[u8]) -> String {
        let signature: Signature = self.signing.sign(data);
        format!("ecdsa-p256:{}", hex::encode(signature.to_bytes()))
    }

    /// Открытый ключ в формате для верификации.
    pub fn verifying_key(&self) -> VerifyingKey {
        *self.signing.verifying_key()
    }
}

/// Проверить подпись `signature_text` (`ecdsa-p256:<hex>`) для `data`
/// открытым ключом `pubkey_hex` (130 hex, несжатый SEC1).
pub fn verify(pubkey_hex: &str, data: &[u8], signature_text: &str) -> IdentityResult<()> {
    let (scheme, sig_hex) = signature_text
        .split_once(':')
        .ok_or_else(|| IdentityError::Key("подпись без схемы".into()))?;
    if scheme != "ecdsa-p256" {
        return Err(IdentityError::Key(format!(
            "неизвестная схема подписи: {scheme}"
        )));
    }
    let sig_bytes =
        hex::decode(sig_hex).map_err(|err| IdentityError::Key(format!("подпись не hex: {err}")))?;
    let signature = Signature::from_slice(&sig_bytes)
        .map_err(|err| IdentityError::Key(format!("подпись не разобрана: {err}")))?;
    let pk_bytes =
        hex::decode(pubkey_hex).map_err(|err| IdentityError::Key(format!("ключ не hex: {err}")))?;
    let point = p256::EncodedPoint::from_bytes(pk_bytes)
        .map_err(|_| IdentityError::Key("ключ не точка P-256".into()))?;
    let vk = VerifyingKey::from_encoded_point(&point)
        .map_err(|err| IdentityError::Key(format!("{err}")))?;
    vk.verify(data, &signature)
        .map_err(|_| IdentityError::BadSignature)
}

// ---- keystore ----

#[derive(serde::Serialize, serde::Deserialize)]
struct KeystoreFile {
    format: String,
    node_id: String,
    pubkey: String,
    /// None — ключ лежит открытым текстом (честно помечено).
    encrypted: Option<EncryptedBlob>,
    /// Открытый hex, если шифрование не используется.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plain_secret: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct EncryptedBlob {
    salt: String,
    nonce: String,
    data: String,
}

/// Сохранить ключ в файл. С парольной фразой — AES-256-GCM(PBKDF2(фраза)).
/// Без — открытый hex и предупреждение в лог вызывающего.
pub fn save_keystore(
    path: &std::path::Path,
    key: &KeyPair,
    passphrase: Option<&str>,
) -> IdentityResult<()> {
    let file = match passphrase {
        Some(pass) if !pass.is_empty() => {
            let mut salt = [0u8; 16];
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut salt);
            let mut aes_key = [0u8; 32];
            pbkdf2_hmac::<Sha256>(pass.as_bytes(), &salt, PBKDF2_ROUNDS, &mut aes_key);
            let cipher_key: Key<Aes256Gcm> = aes_key.into();
            let cipher = Aes256Gcm::new(&cipher_key);
            let mut nonce_bytes = [0u8; 12];
            AeadOsRng.fill_bytes(&mut nonce_bytes);
            let nonce: Nonce<U12> = nonce_bytes.into();
            let data = cipher
                .encrypt(&nonce, key.secret_bytes().as_ref())
                .map_err(|_| IdentityError::Keystore("шифрование не удалось".into()))?;
            KeystoreFile {
                format: "pactum-keystore/v1".into(),
                node_id: key.node_id(),
                pubkey: key.pubkey_hex(),
                encrypted: Some(EncryptedBlob {
                    salt: hex::encode(salt),
                    nonce: hex::encode(nonce_bytes),
                    data: hex::encode(data),
                }),
                plain_secret: None,
            }
        }
        _ => KeystoreFile {
            format: "pactum-keystore/v1".into(),
            node_id: key.node_id(),
            pubkey: key.pubkey_hex(),
            encrypted: None,
            plain_secret: Some(hex::encode(key.secret_bytes())),
        },
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&file)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Загрузить ключ из файла. Для зашифрованного нужна фраза.
pub fn load_keystore(path: &std::path::Path, passphrase: Option<&str>) -> IdentityResult<KeyPair> {
    let text = std::fs::read_to_string(path)?;
    let file: KeystoreFile = serde_json::from_str(&text)?;
    if file.format != "pactum-keystore/v1" {
        return Err(IdentityError::Keystore(format!(
            "неизвестный формат: {}",
            file.format
        )));
    }
    if let Some(blob) = file.encrypted {
        let pass = passphrase
            .filter(|p| !p.is_empty())
            .ok_or_else(|| IdentityError::Keystore("хранилище зашифровано — нужна фраза".into()))?;
        let salt = hex::decode(&blob.salt).map_err(|e| IdentityError::Keystore(e.to_string()))?;
        let nonce_bytes =
            hex::decode(&blob.nonce).map_err(|e| IdentityError::Keystore(e.to_string()))?;
        let data = hex::decode(&blob.data).map_err(|e| IdentityError::Keystore(e.to_string()))?;
        let mut aes_key = [0u8; 32];
        pbkdf2_hmac::<Sha256>(pass.as_bytes(), &salt, PBKDF2_ROUNDS, &mut aes_key);
        let cipher_key: Key<Aes256Gcm> = aes_key.into();
        let cipher = Aes256Gcm::new(&cipher_key);
        let nonce_arr: [u8; 12] = nonce_bytes
            .try_into()
            .map_err(|_| IdentityError::Keystore("nonce не 12 байт".into()))?;
        let nonce: Nonce<U12> = nonce_arr.into();
        let plain = cipher
            .decrypt(&nonce, data.as_ref())
            .map_err(|_| IdentityError::Keystore("не расшифровано: неверная фраза?".into()))?;
        KeyPair::from_secret_bytes(&plain)
    } else {
        let secret = file
            .plain_secret
            .ok_or_else(|| IdentityError::Keystore("в хранилище нет ключа".into()))?;
        let bytes = hex::decode(secret).map_err(|e| IdentityError::Keystore(e.to_string()))?;
        KeyPair::from_secret_bytes(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_verify_roundtrip() {
        let key = KeyPair::generate();
        let data = "pactum: контракт c-abc".as_bytes();
        let sig = key.sign(data);
        assert!(verify(&key.pubkey_hex(), data, &sig).is_ok());
        let wrong = "pactum: контракт c-abd".as_bytes();
        assert!(verify(&key.pubkey_hex(), wrong, &sig).is_err());
    }

    #[test]
    fn node_id_is_stable_and_shaped() {
        let key = KeyPair::generate();
        let id = key.node_id();
        assert!(pactum_core::id::is_valid_node_id(&id));
        assert_eq!(key.node_id(), id);
    }

    #[test]
    fn pubkey_is_130_hex() {
        let key = KeyPair::generate();
        assert_eq!(key.pubkey_hex().len(), 130);
    }

    #[test]
    fn keystore_plain_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pactum-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.json");
        let key = KeyPair::generate();
        save_keystore(&path, &key, None).unwrap();
        let back = load_keystore(&path, None).unwrap();
        assert_eq!(back.node_id(), key.node_id());
        assert_eq!(back.pubkey_hex(), key.pubkey_hex());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn keystore_encrypted_roundtrip_and_wrong_pass() {
        let dir = std::env::temp_dir().join(format!("pactum-ide-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key.json");
        let key = KeyPair::generate();
        save_keystore(&path, &key, Some("париж-столица-париж")).unwrap();
        let back = load_keystore(&path, Some("париж-столица-париж")).unwrap();
        assert_eq!(back.node_id(), key.node_id());
        assert!(load_keystore(&path, Some("другая фраза")).is_err());
        assert!(load_keystore(&path, None).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
