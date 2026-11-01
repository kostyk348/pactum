//! Идентификаторы участников и объектов.
//!
//! Участник протокола — не обязательно агент. Это любой автономный узел:
//! человек с ноутбуком, сервис в кластере, датчик на складе, организация,
//! инстанс агента. Идентичность задаётся открытой частью ключа (см.
//! `pactum-identity`), а в тексте протокола участник фигурирует коротким
//! отпечатком — `node id`.
//!
//! Формат: `p` + 16 hex-символов (64 бита отпечатка открытого ключа).
//! Читается человеком, не выдаёт весь ключ, различает узлы.

/// Короткий идентификатор узла: `p` + первые 8 байт sha256 открытого ключа.
pub fn node_id_from_pubkey(pubkey_bytes: &[u8]) -> String {
    let digest = crate::hash::sha256_hex(pubkey_bytes);
    format!("p{}", &digest[..16])
}

/// Проверить форму идентификатора узла.
pub fn is_valid_node_id(value: &str) -> bool {
    value.len() == 17 && value.starts_with('p') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Идентификатор контракта: `c-` + случайные 12 hex.
pub fn new_contract_id() -> String {
    format!("c-{}", crate::id::random_hex(12))
}

/// Идентификатор задачи доставки/подписки и прочих объектов.
pub fn new_object_id(prefix: &str) -> String {
    format!("{prefix}-{}", crate::id::random_hex(10))
}

/// Случайные байты в hex — публичный помощник для идентификаторов.
pub fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_shape() {
        let id = node_id_from_pubkey(b"02abcdef");
        assert!(id.starts_with('p'));
        assert_eq!(id.len(), 17);
        assert!(is_valid_node_id(&id));
        assert!(!is_valid_node_id("agent-1"));
        assert!(!is_valid_node_id("pzz"));
    }

    #[test]
    fn ids_are_unique_enough() {
        let a = new_contract_id();
        let b = new_contract_id();
        assert_ne!(a, b);
        assert!(a.starts_with("c-"));
        assert!(new_object_id("t").starts_with("t-"));
    }
}
