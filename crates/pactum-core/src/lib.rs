//! # pactum-core
//!
//! Ядро протокола ACP (Autonomous Contract Protocol) и его референс-реализации
//! Pactum: то, что нужно любому узлу независимо от роли.
//!
//! * [`id`] — идентификаторы участников и объектов;
//! * [`hash`] — SHA-256 и отпечатки;
//! * [`letter`] — «письмо» (RFC-822-подобный конверт): универсальная оболочка сообщения;
//! * [`schema`] — валидатор JSON по схеме (подмножество JSON Schema Draft-07);
//! * [`facts`] — цепочка фактов, которую нельзя подделать (hash chain);
//! * [`time`] — время и дедлайны;
//! * [`error`] — общие ошибки.
//!
//! Ядро не знает ни про деньги, ни про HTTP, ни про агентов. Участник протокола —
//! что угодно: человек, сервис, устройство, организация или агент.

pub mod error;
pub mod facts;
pub mod hash;
pub mod id;
pub mod letter;
pub mod schema;
pub mod time;

pub use error::CoreError;
pub use letter::{Letter, LetterKind, PROTOCOL_NAME};
pub use time::{now, now_iso, parse_iso};

/// Версия протокола ACP, реализуемая этим ядром.
pub const ACP_VERSION: &str = "acp/1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_stable() {
        assert_eq!(ACP_VERSION, "acp/1");
        assert_eq!(PROTOCOL_NAME, "Pactum");
    }
}
