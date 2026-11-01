//! Escrow-движок и трейт провайдера расчётов.
//!
//! Деньги — это сдерживатель, а не вера. Пока задача не доказана, бюджет
//! заморожен в escrow: никто не может вывести его раньше, а исполнитель
//! получает долю только после проверки. Тайм-аут — слэшинг ставки и минус
//! репутация. Наследие acp-mvp сохранено: +1 за успех, −1 за провал, −5 за
//! тайм-аут.
//!
//! Трейт [`SettlementProvider`] позволяет подставить вместо симулятора
//! внешний провайдер: смарт-контракт (см. `contracts/AgentEscrow.sol` в
//! репозитории), бухгалтерию организации, счётчик кредитов. Ядро сети
//! не знает и не хочет знать, чем именно платят.

use std::collections::BTreeMap;

use thiserror::Error;

/// Ошибки расчётов.
#[derive(Debug, Error, PartialEq)]
pub enum SettlementError {
    #[error("задача {0} не найдена в escrow")]
    UnknownTask(String),
    #[error("задача {0} в состоянии {1}: операция неприменима")]
    BadState(String, &'static str),
    #[error("принять задачу может только неопределённый ещё исполнитель")]
    AlreadyAssigned,
    #[error("дедлайн не задан — слэшинг по тайм-ауту невозможен")]
    NoDeadline,
    #[error("делёж долей должен быть 100, получено {0}")]
    BadSplit(i64),
    #[error("бюджет не число: {0}")]
    BadAmount(String),
}

pub type SettlementResult<T> = Result<T, SettlementError>;

/// Провайдер расчётов. Все операции идемпотентны по contract_id.
///
/// Реализации: [`EscrowEngine`] (встроенный детерминированный симулятор),
/// внешний смарт-контракт, внутренняя бухгалтерия. MVP acp-mvp использовал
/// ровно такой же трюк: симуляция для тестов, блокчейн — в продакшене.
pub trait SettlementProvider {
    /// Заблокировать бюджет заказчика. → `Initiated`.
    fn create_task(
        &mut self,
        contract_id: &str,
        principal: &str,
        budget: &str,
    ) -> SettlementResult<EscrowState>;

    /// Исполнитель взял задачу (и заморозил ставку). → `Accepted`.
    fn accept_task(&mut self, contract_id: &str, executor: &str) -> SettlementResult<EscrowState>;

    /// Исполнитель заявил результат (с хешем исполнения). → `Completed`.
    fn submit_result(
        &mut self,
        contract_id: &str,
        exec_hash: &str,
    ) -> SettlementResult<EscrowState>;

    /// Верdict проверки: true — раздать бюджет по долям (`Released`), false — рефанд (`Refunded`).
    fn verify_and_release(
        &mut self,
        contract_id: &str,
        success: bool,
    ) -> SettlementResult<EscrowState>;

    /// Исполнитель молчит после дедлайна: ставка сгорает, бюджет возвращается. → `Slashed`.
    fn slash_on_timeout(
        &mut self,
        contract_id: &str,
        now_secs: u64,
    ) -> SettlementResult<EscrowState>;

    /// Состояние задачи.
    fn state(&self, contract_id: &str) -> Option<EscrowState>;

    /// Репутация участника.
    fn reputation(&self, participant: &str) -> i64;
}

/// Состояние escrow-задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowState {
    Initiated,
    Accepted,
    Completed,
    Released,
    Refunded,
    Slashed,
    Cancelled,
}

impl EscrowState {
    pub fn as_str(&self) -> &'static str {
        match self {
            EscrowState::Initiated => "Initiated",
            EscrowState::Accepted => "Accepted",
            EscrowState::Completed => "Completed",
            EscrowState::Released => "Released",
            EscrowState::Refunded => "Refunded",
            EscrowState::Slashed => "Slashed",
            EscrowState::Cancelled => "Cancelled",
        }
    }
}

/// Запись escrow по одному контракту.
#[derive(Debug, Clone, PartialEq)]
pub struct EscrowRecord {
    pub contract_id: String,
    pub principal: String,
    pub executor: Option<String>,
    /// Замороженный бюджет: "10.00 USDC".
    pub budget: String,
    /// Ставка исполнителя, замороженная при accept.
    pub stake: String,
    /// Хеш исполнения из submit_result.
    pub exec_hash: Option<String>,
    pub state: EscrowState,
}

/// Событие репутации (журнал для людей).
#[derive(Debug, Clone, PartialEq)]
pub struct ReputationEvent {
    pub participant: String,
    pub delta: i64,
    pub reason: String,
    pub contract_id: String,
}

/// Репутация: счётчики + история.
#[derive(Debug, Clone, Default)]
pub struct Reputation {
    scores: BTreeMap<String, i64>,
    pub events: Vec<ReputationEvent>,
}

impl Reputation {
    pub fn bump(&mut self, participant: &str, delta: i64, reason: &str, contract_id: &str) {
        *self.scores.entry(participant.to_string()).or_insert(0) += delta;
        self.events.push(ReputationEvent {
            participant: participant.to_string(),
            delta,
            reason: reason.to_string(),
            contract_id: contract_id.to_string(),
        });
    }

    pub fn score(&self, participant: &str) -> i64 {
        self.scores.get(participant).copied().unwrap_or(0)
    }

    pub fn all(&self) -> &BTreeMap<String, i64> {
        &self.scores
    }
}

/// Встроенный escrow-движок: детерминированный симулятор расчётов.
///
/// Умеет всё то же, что смарт-контракт-референс: полный жизненный цикл,
/// делёж royalty, слэшинг ставки по тайм-ауту, репутация. Работает без
/// сети и без блокчейна — для локальных сделок, тестов и CI.
#[derive(Debug, Default)]
pub struct EscrowEngine {
    tasks: BTreeMap<String, EscrowRecord>,
    /// Дедлайны для slash_on_timeout (из контракта).
    deadlines: BTreeMap<String, u64>,
    /// Доли royalty из контракта.
    splits: BTreeMap<String, (i64, i64)>,
    /// Процент сгорания ставки при тайм-ауте.
    slash_pcts: BTreeMap<String, i64>,
    pub reputation: Reputation,
}

impl EscrowEngine {
    pub fn new() -> EscrowEngine {
        EscrowEngine::default()
    }

    /// Зарегистрировать параметры сделки (дедлайн, доли, слэшинг) из контракта.
    pub fn register(
        &mut self,
        contract_id: &str,
        deadline_secs: Option<u64>,
        split: (i64, i64),
        slash_pct: i64,
    ) -> SettlementResult<()> {
        if split.0 + split.1 != 100 {
            return Err(SettlementError::BadSplit(split.0 + split.1));
        }
        self.splits.insert(contract_id.to_string(), split);
        self.slash_pcts.insert(contract_id.to_string(), slash_pct);
        if let Some(d) = deadline_secs {
            self.deadlines.insert(contract_id.to_string(), d);
        }
        Ok(())
    }

    /// Все записи (для API и журналов).
    pub fn records(&self) -> Vec<&EscrowRecord> {
        self.tasks.values().collect()
    }

    /// Запись по контракту.
    pub fn record(&self, contract_id: &str) -> Option<&EscrowRecord> {
        self.tasks.get(contract_id)
    }

    /// Тик обслуживания: слэшинг всех просроченных задач в Accepted.
    /// Возвращает список «просрочил и наказан».
    pub fn tick(&mut self, now_secs: u64) -> Vec<String> {
        let mut slashed = Vec::new();
        let expired: Vec<String> = self
            .tasks
            .iter()
            .filter(|(id, rec)| {
                rec.state == EscrowState::Accepted
                    && self
                        .deadlines
                        .get(*id)
                        .map(|d| *d < now_secs)
                        .unwrap_or(false)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if self.slash_on_timeout(&id, now_secs).is_ok() {
                slashed.push(id);
            }
        }
        slashed
    }
}

impl SettlementProvider for EscrowEngine {
    fn create_task(
        &mut self,
        contract_id: &str,
        principal: &str,
        budget: &str,
    ) -> SettlementResult<EscrowState> {
        if self.tasks.contains_key(contract_id) {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                "уже создана",
            ));
        }
        self.tasks.insert(
            contract_id.to_string(),
            EscrowRecord {
                contract_id: contract_id.to_string(),
                principal: principal.to_string(),
                executor: None,
                budget: budget.to_string(),
                stake: String::new(),
                exec_hash: None,
                state: EscrowState::Initiated,
            },
        );
        Ok(EscrowState::Initiated)
    }

    fn accept_task(&mut self, contract_id: &str, executor: &str) -> SettlementResult<EscrowState> {
        let rec = self
            .tasks
            .get_mut(contract_id)
            .ok_or_else(|| SettlementError::UnknownTask(contract_id.to_string()))?;
        if rec.executor.is_some() {
            return Err(SettlementError::AlreadyAssigned);
        }
        if rec.state != EscrowState::Initiated {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                rec.state.as_str(),
            ));
        }
        rec.executor = Some(executor.to_string());
        rec.state = EscrowState::Accepted;
        rec.stake = format!(
            "ставка {} ({}%)",
            rec.budget,
            self.slash_pcts.get(contract_id).copied().unwrap_or(10)
        );
        Ok(EscrowState::Accepted)
    }

    fn submit_result(
        &mut self,
        contract_id: &str,
        exec_hash: &str,
    ) -> SettlementResult<EscrowState> {
        let rec = self
            .tasks
            .get_mut(contract_id)
            .ok_or_else(|| SettlementError::UnknownTask(contract_id.to_string()))?;
        if rec.state != EscrowState::Accepted {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                rec.state.as_str(),
            ));
        }
        rec.exec_hash = Some(exec_hash.to_string());
        rec.state = EscrowState::Completed;
        Ok(EscrowState::Completed)
    }

    fn verify_and_release(
        &mut self,
        contract_id: &str,
        success: bool,
    ) -> SettlementResult<EscrowState> {
        let rec = self
            .tasks
            .get_mut(contract_id)
            .ok_or_else(|| SettlementError::UnknownTask(contract_id.to_string()))?;
        if rec.state != EscrowState::Completed {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                rec.state.as_str(),
            ));
        }
        if success {
            let (principal_pct, executor_pct) =
                self.splits.get(contract_id).copied().unwrap_or((90, 10));
            if principal_pct + executor_pct != 100 {
                return Err(SettlementError::BadSplit(principal_pct + executor_pct));
            }
            rec.state = EscrowState::Released;
            if let Some(executor) = rec.executor.clone() {
                self.reputation
                    .bump(&executor, 1, "задача доказана", contract_id);
            }
            Ok(EscrowState::Released)
        } else {
            rec.state = EscrowState::Refunded;
            if let Some(executor) = rec.executor.clone() {
                self.reputation
                    .bump(&executor, -1, "результат не принят", contract_id);
            }
            Ok(EscrowState::Refunded)
        }
    }

    fn slash_on_timeout(
        &mut self,
        contract_id: &str,
        now_secs: u64,
    ) -> SettlementResult<EscrowState> {
        let deadline = self
            .deadlines
            .get(contract_id)
            .copied()
            .ok_or(SettlementError::NoDeadline)?;
        if now_secs <= deadline {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                "дедлайн ещё не наступил",
            ));
        }
        let rec = self
            .tasks
            .get_mut(contract_id)
            .ok_or_else(|| SettlementError::UnknownTask(contract_id.to_string()))?;
        if rec.state != EscrowState::Accepted {
            return Err(SettlementError::BadState(
                contract_id.to_string(),
                rec.state.as_str(),
            ));
        }
        rec.state = EscrowState::Slashed;
        if let Some(executor) = rec.executor.clone() {
            self.reputation
                .bump(&executor, -5, "тишина после дедлайна", contract_id);
        }
        Ok(EscrowState::Slashed)
    }

    fn state(&self, contract_id: &str) -> Option<EscrowState> {
        self.tasks.get(contract_id).map(|r| r.state)
    }

    fn reputation(&self, participant: &str) -> i64 {
        self.reputation.score(participant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> EscrowEngine {
        let mut e = EscrowEngine::new();
        e.register("c-1", Some(1_000), (90, 10), 10).unwrap();
        e
    }

    #[test]
    fn happy_path_releases_and_reputes() {
        let mut e = engine();
        assert_eq!(
            e.create_task("c-1", "p-principal", "10.00 USDC"),
            Ok(EscrowState::Initiated)
        );
        assert_eq!(
            e.accept_task("c-1", "p-executor"),
            Ok(EscrowState::Accepted)
        );
        assert_eq!(e.submit_result("c-1", "abc123"), Ok(EscrowState::Completed));
        assert_eq!(e.verify_and_release("c-1", true), Ok(EscrowState::Released));
        assert_eq!(e.reputation("p-executor"), 1);
        assert_eq!(
            e.record("c-1").unwrap().exec_hash.as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn failure_refunds_and_downreputes() {
        let mut e = engine();
        e.create_task("c-1", "p-principal", "10.00 USDC").unwrap();
        e.accept_task("c-1", "p-executor").unwrap();
        e.submit_result("c-1", "abc").unwrap();
        assert_eq!(
            e.verify_and_release("c-1", false),
            Ok(EscrowState::Refunded)
        );
        assert_eq!(e.reputation("p-executor"), -1);
    }

    #[test]
    fn timeout_slashes_five() {
        let mut e = engine();
        e.create_task("c-1", "p-principal", "10.00 USDC").unwrap();
        e.accept_task("c-1", "p-executor").unwrap();
        assert_eq!(
            e.slash_on_timeout("c-1", 1_000),
            Err(SettlementError::BadState(
                "c-1".into(),
                "дедлайн ещё не наступил"
            ))
        );
        assert_eq!(e.slash_on_timeout("c-1", 1_001), Ok(EscrowState::Slashed));
        assert_eq!(e.reputation("p-executor"), -5);
    }

    #[test]
    fn tick_slashes_everything_expired() {
        let mut e = engine();
        e.create_task("c-1", "p1", "1 USDC").unwrap();
        e.create_task("c-2", "p1", "1 USDC").unwrap();
        e.register("c-2", Some(10_000), (90, 10), 10).unwrap();
        e.accept_task("c-1", "p-exec").unwrap();
        e.accept_task("c-2", "p-exec").unwrap();
        let slashed = e.tick(3_000);
        assert_eq!(slashed, vec!["c-1".to_string()]);
        assert_eq!(e.state("c-2"), Some(EscrowState::Accepted));
    }

    #[test]
    fn cannot_steal_before_submit() {
        let mut e = engine();
        e.create_task("c-1", "p-principal", "10.00 USDC").unwrap();
        // заказчик не может «освободить» escrow до submit_result
        assert!(e.verify_and_release("c-1", true).is_err());
        // двойной accept невозможен
        e.accept_task("c-1", "p-exec").unwrap();
        assert_eq!(
            e.accept_task("c-1", "p-exec2"),
            Err(SettlementError::AlreadyAssigned)
        );
    }

    #[test]
    fn double_create_rejected() {
        let mut e = engine();
        e.create_task("c-1", "p1", "1 USDC").unwrap();
        assert!(e.create_task("c-1", "p1", "1 USDC").is_err());
    }
}
