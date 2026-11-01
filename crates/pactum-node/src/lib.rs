//! # pactum-node
//!
//! Узел сети ACP: оркестратор + песочница + REST API + capability-relay.
//!
//! Наследие acp-mvp в универсальном виде. Узел — не «агентский инстанс», а
//! точка присутствия любого участника: у сервиса узел живёт в кластере, у
//! человека — в ноутбуке, у агента — в его инстансе.
//!
//! Роли узла:
//!
//! * **principal** (заказчик) — публикует контракты, принимает доказательства,
//!   проверяет и высвобождает escrow;
//! * **executor** (исполнитель) — подписан на capability, принимает контракты,
//!   исполняет в песочнице, отчитывается доказательством.
//!
//! Роли не привязаны к типу участника: человек может быть исполнителем,
//! сервис — заказчиком, агент — и тем и другим.

pub mod api;
pub mod orchestrator;
pub mod relay;
pub mod sandbox;

pub use orchestrator::{NodeCore, NodeOptions, Outcome, Role};
pub use relay::{RelayRegistry, RelayState};
pub use sandbox::{ProofDocument, Sandbox};
