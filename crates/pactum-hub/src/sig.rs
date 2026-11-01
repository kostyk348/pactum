//! HMAC-SHA256 для подписи доставок (`X-Hub-Signature: sha256=…`), как в hubd.
//! Конструкция HMAC — RFC 2104, реализовано на sha2 без лишних крейтов.

use sha2::{Digest, Sha256};

const BLOCK: usize = 64;

pub fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let mut hasher = Sha256::new();
        hasher.update(key);
        key_block[..32].copy_from_slice(&hasher.finalize());
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// Заголовок `X-Hub-Signature: sha256=<hex>` для доставки подписчику.
pub fn hub_signature(secret: &str, body: &[u8]) -> String {
    format!(
        "sha256={}",
        hex::encode(hmac_sha256(secret.as_bytes(), body))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4231_case1() {
        // RFC 4231, тест 1: ключ = 0x0b * 20, данные "Hi There"
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex::encode(mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn rfc4231_case2() {
        let mac = hmac_sha256(b"Jefe", "what do ya want for nothing?".as_bytes());
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn long_key_is_hashed() {
        // ключ длиннее блока — хешируется, RFC 4231 тест 6
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            "Test Using Larger Than Block-Size Key - Hash Key First".as_bytes(),
        );
        assert_eq!(
            hex::encode(mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signature_header_shape() {
        let sig = hub_signature("secret", b"body");
        assert!(sig.starts_with("sha256="));
        assert_eq!(sig.len(), 7 + 64);
    }
}
