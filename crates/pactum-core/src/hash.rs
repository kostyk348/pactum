//! SHA-256 и отпечатки. Всё, что в Pactum утверждается, — хешируется.

use sha2::{Digest, Sha256};

/// SHA-256 от байтов, hex-строка в нижнем регистре.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// SHA-256 от текста (UTF-8), hex.
pub fn sha256_text(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

/// Исполнение команды для доказательства: SHA-256 от stdout+stderr вместе.
pub fn exec_hash(stdout: &[u8], stderr: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(stdout);
    hasher.update(stderr);
    hex::encode(hasher.finalize())
}

/// FNV-1a 64 — не криптография, а сравнение выводов (как в `proof`).
/// Отпечаток нормализованного вывода: быстро, стабильно, читаемо.
pub fn fnv1a64(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(
            sha256_text(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_text("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn fnv1a64_known_vector() {
        // известный вектор FNV-1a 64 для пустой строки
        assert_eq!(fnv1a64(""), "cbf29ce484222325");
        assert_eq!(fnv1a64("a"), "af63dc4c8601ec8c");
    }

    #[test]
    fn exec_hash_order_matters() {
        assert_ne!(exec_hash(b"out", b"err"), exec_hash(b"err", b"out"));
        assert_eq!(exec_hash(b"out", b"err"), exec_hash(b"out", b"err"));
    }
}
