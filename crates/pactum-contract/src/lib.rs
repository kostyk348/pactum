//! # pactum-contract
//!
//! Контракт — первичная сущность сети ACP. Это машиночитаемое обязательство
//! между двумя участниками: «сделай X, вот как я проверю, вот что ты получишь».
//!
//! Универсальность здесь буквальная: под X подходит что угодно — сборка кода
//! агентом, деплой сервиса, поставка данных с датчика, перевод текста,
//! еженедельный бэкап руками человека. Проверка — команды и артефакты.
//! Расчёты — escrow-движок (встроенный симулятор или внешний провайдер,
//! например смарт-контракт на блокчейне — см. `contracts/` в репозитории).
//!
//! Жизненный цикл сделки (наследие acp-mvp, сохранён полностью):
//!
//! ```text
//! Initiated ──accept──▶ Accepted ──submit──▶ Completed ──release(true)──▶ Released
//!     │                     │                      └──release(false)──▶ Refunded
//!     │                     └──(дедлайн истёк)──▶ Slashed
//!     └──(отменён до accept)──▶ Cancelled
//! ```

pub mod escrow;
pub mod model;
pub mod settlement;

pub use escrow::{
    EscrowEngine, EscrowRecord, EscrowState, Reputation, SettlementError, SettlementProvider,
};
pub use model::{
    ArtifactExpect, Budget, Contract, Intent, Parties, RoyaltySplit, Settlement, SourceRef,
    TestCmd, Verification,
};
