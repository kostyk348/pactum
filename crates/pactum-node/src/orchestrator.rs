//! Оркестратор — мозг узла: приём писем, решение, исполнение, доказательства.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use pactum_contract::{Contract, EscrowEngine, EscrowState, SettlementProvider};
use pactum_core::facts::{FactChain, Register};
use pactum_core::letter::{Letter, LetterKind};
use pactum_identity::KeyPair;
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("письмо повреждено: {0}")]
    Letter(String),
    #[error("контракт: {0}")]
    Contract(String),
    #[error("расчёты: {0}")]
    Settlement(String),
    #[error("песочница: {0}")]
    Sandbox(String),
    #[error("поток заблокирован")]
    Poisoned,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type NodeResult<T> = Result<T, NodeError>;

/// Роль узла. Универсальные имена вместо «агент/человек».
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Заказчик: публикует контракты и проверяет доказательства.
    Principal,
    /// Исполнитель: берёт контракты по capability и доказывает выполнение.
    Executor,
}

/// Итог обработки одного письма.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Outcome {
    /// Контракт принят и исполнен, доказательство готово к отправке.
    Executed {
        contract_id: String,
        proof: crate::sandbox::ProofDocument,
        reply: Letter,
    },
    /// Контракт отклонён — с честной причиной.
    Rejected { contract_id: String, reason: String },
    /// Доказательство проверено, escrow высвобожден.
    Verified { contract_id: String, success: bool },
}

/// Настройки узла.
#[derive(Debug, Clone)]
pub struct NodeOptions {
    /// Capability, на которые узел подписан (для исполнителя). Wildcard `*`.
    pub capabilities: Vec<String>,
    /// Минимальный бюджет, ниже которого контракт отвергается (антиспам).
    pub min_budget_units: f64,
    /// Рабочая папка исполнений.
    pub workdir: PathBuf,
}

impl Default for NodeOptions {
    fn default() -> Self {
        NodeOptions {
            capabilities: vec!["*".to_string()],
            min_budget_units: 0.01,
            workdir: std::env::temp_dir().join("pactum-node-work"),
        }
    }
}

/// Ядро узла. Совместно используемое состояние — за Mutex'ами; escrow-движок
/// может быть общим для узлов в одном процессе (локальные сделки без сети).
pub struct NodeCore {
    pub identity: KeyPair,
    pub role: Role,
    pub options: NodeOptions,
    pub engine: Arc<Mutex<EscrowEngine>>,
    pub facts: Mutex<FactChain>,
    inbox: Mutex<Vec<Letter>>,
    contracts: Mutex<BTreeMap<String, Contract>>,
    pub sandbox: Arc<dyn crate::sandbox::Sandbox>,
}

impl NodeCore {
    pub fn new(identity: KeyPair, role: Role, options: NodeOptions) -> NodeCore {
        NodeCore::with_engine(
            identity,
            role,
            options,
            Arc::new(Mutex::new(EscrowEngine::new())),
        )
    }

    /// Узел с общим escrow-движком: локальные сделки нескольких узлов
    /// в одном процессе без сети.
    pub fn with_engine(
        identity: KeyPair,
        role: Role,
        options: NodeOptions,
        engine: Arc<Mutex<EscrowEngine>>,
    ) -> NodeCore {
        NodeCore {
            identity,
            role,
            options,
            engine,
            facts: Mutex::new(FactChain::new()),
            inbox: Mutex::new(Vec::new()),
            contracts: Mutex::new(BTreeMap::new()),
            sandbox: Arc::new(crate::sandbox::LocalSandbox::default()),
        }
    }

    pub fn node_id(&self) -> String {
        self.identity.node_id()
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn crate::sandbox::Sandbox>) -> NodeCore {
        self.sandbox = sandbox;
        self
    }

    /// Подписан ли узел на эту capability (wildcard `*`, суффикс `prefix.*`).
    pub fn matches_capability(&self, capability: &str) -> bool {
        self.options.capabilities.iter().any(|pattern| {
            pattern == "*" || pattern == capability || {
                let prefix = pattern.to_string();
                pattern.ends_with('*') && capability.starts_with(prefix.trim_end_matches('*'))
            }
        })
    }

    /// Принять письмо во входящие (сетевой вход узла).
    pub fn receive(&self, letter: Letter) -> NodeResult<String> {
        if letter.kind().is_none() {
            return Err(NodeError::Letter("письмо без X-Pactum-Kind".into()));
        }
        let id = letter.fingerprint();
        let from = letter.from().unwrap_or("?").to_string();
        let kind = letter
            .kind()
            .map(|k| k.as_str().to_string())
            .unwrap_or_default();
        self.inbox
            .lock()
            .map_err(|_| NodeError::Poisoned)?
            .push(letter);
        self.record_fact(
            Register::Sense,
            &format!("получено письмо {kind} от {from}"),
            None,
        );
        Ok(id)
    }

    /// Сколько писем ждёт обработки.
    pub fn inbox_len(&self) -> usize {
        self.inbox.lock().map(|b| b.len()).unwrap_or(0)
    }

    /// Обработать входящие. Возвращает итоги по каждому разобранному письму.
    pub fn process_inbox(&self) -> NodeResult<Vec<Outcome>> {
        let letters: Vec<Letter> = {
            let mut inbox = self.inbox.lock().map_err(|_| NodeError::Poisoned)?;
            std::mem::take(&mut *inbox)
        };
        let mut outcomes = Vec::new();
        for letter in letters {
            match letter.kind() {
                Some(LetterKind::Contract) => outcomes.push(self.handle_contract(letter)?),
                Some(LetterKind::Proof) => outcomes.push(self.handle_proof(letter)?),
                _ => {} // события потока и квитанции обрабатываются подписчиками
            }
        }
        Ok(outcomes)
    }

    /// Исполнитель: принять контракт, исполнить, подготовить доказательство.
    fn handle_contract(&self, letter: Letter) -> NodeResult<Outcome> {
        let contract = Contract::from_value(&letter.body)
            .map_err(|err| NodeError::Contract(err.to_string()))?;
        let contract_id = contract.contract_id.clone().unwrap_or_default();

        // антиспам: capability и бюджет
        if !self.matches_capability(contract.capability()) {
            return Ok(Outcome::Rejected {
                contract_id,
                reason: format!("capability {} не в подписках узла", contract.capability()),
            });
        }
        if let Ok(amount) = contract.settlement.budget.amount.parse::<f64>() {
            if amount < self.options.min_budget_units {
                return Ok(Outcome::Rejected {
                    contract_id,
                    reason: format!("бюджет {} ниже минимума узла", contract.budget_display()),
                });
            }
        }

        self.contracts
            .lock()
            .map_err(|_| NodeError::Poisoned)?
            .insert(contract_id.clone(), contract.clone());

        // escrow: регистрируем параметры и принимаем задачу
        {
            let mut engine = self.engine.lock().map_err(|_| NodeError::Poisoned)?;
            engine
                .register(
                    &contract_id,
                    contract.deadline_secs(),
                    (
                        contract.settlement.royalty_split.principal,
                        contract.settlement.royalty_split.executor,
                    ),
                    contract.settlement.slash_stake_percent,
                )
                .map_err(|err| NodeError::Settlement(err.to_string()))?;
            // при общем движке задача уже создана заказчиком — не создаём второй раз
            if engine.state(&contract_id).is_none() {
                engine
                    .create_task(
                        &contract_id,
                        &contract.parties.principal,
                        &contract.budget_display(),
                    )
                    .map_err(|err| NodeError::Settlement(err.to_string()))?;
            }
            // повторное принятие — честный отказ, а не ошибка узла
            if let Some(existing) = engine.state(&contract_id) {
                if existing != EscrowState::Initiated {
                    return Ok(Outcome::Rejected {
                        contract_id,
                        reason: format!(
                            "контракт в состоянии {} — уже взят кем-то или завершён",
                            existing.as_str()
                        ),
                    });
                }
            }
            engine
                .accept_task(&contract_id, &self.node_id())
                .map_err(|err| NodeError::Settlement(err.to_string()))?;
        }
        self.record_fact(
            Register::Action,
            &format!("принят контракт {contract_id} ({})", contract.capability()),
            Some(&contract_id),
        );

        // исполнение в песочнице
        let workdir = self.options.workdir.join(&contract_id);
        std::fs::create_dir_all(&workdir).map_err(NodeError::from)?;
        let proof = self
            .sandbox
            .execute(&workdir, &contract.verification)
            .map_err(|err| NodeError::Sandbox(err.to_string()))?;
        self.record_fact(
            Register::Fact,
            &format!(
                "исполнение {contract_id}: exit-коды {:?}, ок={}",
                proof.exit_codes, proof.ok
            ),
            Some(&contract_id),
        );

        // escrow: заявляем результат
        {
            let mut engine = self.engine.lock().map_err(|_| NodeError::Poisoned)?;
            engine
                .submit_result(&contract_id, &proof.exec_hash)
                .map_err(|err| NodeError::Settlement(err.to_string()))?;
        }

        // ответное письмо-доказательство заказчику
        let mut proof_letter = Letter::new(
            LetterKind::Proof,
            &self.node_id(),
            serde_json::json!({
                "contract_id": contract_id,
                "exec_hash": proof.exec_hash,
                "exit_codes": proof.exit_codes,
                "artifacts": proof.artifacts,
                "duration_ms": proof.duration_ms,
                "ok": proof.ok,
                "excerpt": proof.excerpt
            }),
        );
        if let Some(reply_to) = letter.header("X-Pactum-Reply-To") {
            proof_letter.set_header("X-Pactum-Reply-To", reply_to);
        }
        proof_letter.set_header(
            "X-Pactum-Signature",
            &self.identity.sign(proof_letter.signing_bytes().as_slice()),
        );

        Ok(Outcome::Executed {
            contract_id,
            proof,
            reply: proof_letter,
        })
    }

    /// Заказчик: проверить доказательство и высвободить escrow.
    fn handle_proof(&self, letter: Letter) -> NodeResult<Outcome> {
        let contract_id = letter
            .body
            .get("contract_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let ok = letter
            .body
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let exec_hash = letter
            .body
            .get("exec_hash")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        // автопроверка по правилам контракта: proof связан с артефактом и кодами
        if exec_hash.is_empty() {
            return Ok(Outcome::Verified {
                contract_id,
                success: false,
            });
        }
        let mut engine = self.engine.lock().map_err(|_| NodeError::Poisoned)?;
        let state = engine
            .verify_and_release(&contract_id, ok)
            .map_err(|err| NodeError::Settlement(err.to_string()))?;
        drop(engine);
        self.record_fact(
            Register::Logic,
            &format!("доказательство {contract_id} проверено: {}", state.as_str()),
            Some(&contract_id),
        );
        self.record_fact(
            Register::Fact,
            &format!("итог сделки {contract_id}: {}", state.as_str()),
            Some(&contract_id),
        );
        Ok(Outcome::Verified {
            contract_id,
            success: ok,
        })
    }

    /// Заказчик: опубликовать контракт — валидация, escrow.create, письмо.
    pub fn publish_contract(&self, mut contract: Contract) -> NodeResult<Letter> {
        if contract.contract_id.is_none() {
            contract.contract_id = Some(pactum_core::id::new_contract_id());
        }
        contract.parties.principal = self.node_id();
        contract
            .validate()
            .map_err(|err| NodeError::Contract(err.to_string()))?;
        let contract_id = contract.contract_id.clone().unwrap_or_default();
        {
            let mut engine = self.engine.lock().map_err(|_| NodeError::Poisoned)?;
            engine
                .register(
                    &contract_id,
                    contract.deadline_secs(),
                    (
                        contract.settlement.royalty_split.principal,
                        contract.settlement.royalty_split.executor,
                    ),
                    contract.settlement.slash_stake_percent,
                )
                .map_err(|err| NodeError::Settlement(err.to_string()))?;
            engine
                .create_task(&contract_id, &self.node_id(), &contract.budget_display())
                .map_err(|err| NodeError::Settlement(err.to_string()))?;
        }
        self.contracts
            .lock()
            .map_err(|_| NodeError::Poisoned)?
            .insert(contract_id.clone(), contract.clone());
        self.record_fact(
            Register::Action,
            &format!(
                "опубликован контракт {contract_id}: {}",
                contract.capability()
            ),
            Some(&contract_id),
        );

        let mut letter = Letter::new(
            LetterKind::Contract,
            &self.node_id(),
            contract
                .to_value()
                .map_err(|err| NodeError::Contract(err.to_string()))?,
        );
        letter.set_header("X-Pactum-Capability", contract.capability());
        letter.set_header("X-Escrow-Budget", &contract.budget_display());
        if let Some(deadline) = &contract.deadline_utc {
            letter.set_header("X-Timeout-UTC", deadline);
        }
        letter.set_header(
            "X-Pactum-Signature",
            &self.identity.sign(letter.signing_bytes().as_slice()),
        );
        Ok(letter)
    }

    /// Запомнить контракт (после публикации или получения).
    pub fn remember_contract(&self, contract: Contract) {
        if let Some(id) = contract.contract_id.clone() {
            if let Ok(mut map) = self.contracts.lock() {
                map.insert(id, contract);
            }
        }
    }

    /// Список известных контрактов.
    pub fn known_contracts(&self) -> Vec<Contract> {
        self.contracts
            .lock()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Состояние escrow по контракту.
    pub fn escrow_state(&self, contract_id: &str) -> Option<EscrowState> {
        self.engine.lock().ok()?.state(contract_id)
    }

    /// Репутация участника.
    pub fn reputation(&self, participant: &str) -> i64 {
        self.engine
            .lock()
            .map(|e| e.reputation.score(participant))
            .unwrap_or(0)
    }

    /// Записать факт в цепочку памяти.
    pub fn record_fact(&self, register: Register, text: &str, contract_id: Option<&str>) {
        if let Ok(mut chain) = self.facts.lock() {
            chain.append(register, text, contract_id);
        }
    }

    /// Проверить цепочку памяти.
    pub fn verify_memory(&self) -> NodeResult<()> {
        self.facts
            .lock()
            .map_err(|_| NodeError::Poisoned)?
            .verify()
            .map_err(|err| NodeError::Contract(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pactum_contract::{
        Budget, Intent, RoyaltySplit, Settlement, SourceRef, TestCmd, Verification,
    };

    fn sample_contract(principal: &str) -> Contract {
        Contract {
            schema: "pactum-contract/v1".into(),
            schema_version: "1.0.0".into(),
            contract_id: Some("c-test0001".into()),
            nonce: None,
            intent: Intent {
                kind: "ci.compile.rust".into(),
                domain: "rust".into(),
                title: "Проверка".into(),
                source: SourceRef {
                    kind: "inline".into(),
                    data: None,
                    reference: None,
                },
            },
            verification: Verification {
                tests: vec![TestCmd {
                    cmd: "echo готово".into(),
                    timeout_s: 30,
                }],
                artifacts_expected: vec![],
            },
            settlement: Settlement {
                provider: "sim".into(),
                budget: Budget {
                    amount: "1.00".into(),
                    unit: "USDC".into(),
                },
                on_success: "release_escrow".into(),
                on_failure: "refund".into(),
                royalty_split: RoyaltySplit {
                    principal: 90,
                    executor: 10,
                },
                auto_verify: true,
                slash_stake_percent: 10,
            },
            parties: pactum_contract::Parties {
                principal: principal.into(),
                executor: None,
            },
            deadline_utc: None,
            notes: None,
        }
    }

    fn node(role: Role, capabilities: Vec<&str>) -> NodeCore {
        let options = NodeOptions {
            capabilities: capabilities.into_iter().map(str::to_string).collect(),
            ..Default::default()
        };
        NodeCore::new(KeyPair::generate(), role, options)
    }

    #[test]
    fn capability_matching_with_wildcards() {
        let n = node(Role::Executor, vec!["ci.compile.*", "deploy"]);
        assert!(n.matches_capability("ci.compile.rust"));
        assert!(n.matches_capability("deploy"));
        assert!(!n.matches_capability("crypto.reverse"));
        let star = node(Role::Executor, vec!["*"]);
        assert!(star.matches_capability("что.угодно.тут"));
    }

    #[test]
    fn full_local_lifecycle() {
        let shared: Arc<Mutex<EscrowEngine>> = Arc::new(Mutex::new(EscrowEngine::new()));
        let customer = NodeCore::with_engine(
            KeyPair::generate(),
            Role::Principal,
            NodeOptions {
                capabilities: vec![],
                ..Default::default()
            },
            shared.clone(),
        );
        let executor = NodeCore::with_engine(
            KeyPair::generate(),
            Role::Executor,
            NodeOptions {
                capabilities: vec!["ci.compile.*".into()],
                ..Default::default()
            },
            shared,
        );

        let contract = sample_contract(&customer.node_id());
        let letter = customer.publish_contract(contract).expect("публикация");
        assert_eq!(
            customer.escrow_state("c-test0001"),
            Some(EscrowState::Initiated)
        );

        // исполнитель получает
        executor.receive(letter).unwrap();
        let outcomes = executor.process_inbox().unwrap();
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            Outcome::Executed {
                contract_id, proof, ..
            } => {
                assert_eq!(contract_id, "c-test0001");
                assert!(proof.ok);
            }
            other => panic!("ожидали Executed, получено {other:?}"),
        }
        assert!(executor.verify_memory().is_ok());

        // заказчик получает доказательство (вручную передаём письмо)
        let proof_letter = Letter::new(
            LetterKind::Proof,
            &executor.node_id(),
            serde_json::json!({
                "contract_id": "c-test0001",
                "exec_hash": "abc",
                "ok": true
            }),
        );
        customer.receive(proof_letter).unwrap();
        let outcomes = customer.process_inbox().unwrap();
        assert!(matches!(
            &outcomes[0],
            Outcome::Verified { success: true, .. }
        ));
        assert_eq!(
            customer.escrow_state("c-test0001"),
            Some(EscrowState::Released)
        );
        assert_eq!(customer.reputation(&executor.node_id()), 1);
    }

    #[test]
    fn rejects_wrong_capability_honestly() {
        let executor = node(Role::Executor, vec!["deploy"]);
        let customer = node(Role::Principal, vec![]);
        let contract = sample_contract(&customer.node_id());
        let letter = customer.publish_contract(contract).unwrap();
        executor.receive(letter).unwrap();
        let outcomes = executor.process_inbox().unwrap();
        match &outcomes[0] {
            Outcome::Rejected { reason, .. } => assert!(reason.contains("capability"), "{reason}"),
            other => panic!("ожидали Rejected, получено {other:?}"),
        }
    }

    #[test]
    fn rejects_low_budget() {
        let executor = node(Role::Executor, vec!["*"]);
        let customer = node(Role::Principal, vec![]);
        let mut contract = sample_contract(&customer.node_id());
        contract.settlement.budget.amount = "0.001".into();
        let letter = customer.publish_contract(contract).unwrap();
        executor.receive(letter).unwrap();
        let outcomes = executor.process_inbox().unwrap();
        match &outcomes[0] {
            Outcome::Rejected { reason, .. } => assert!(reason.contains("ниже минимума")),
            other => panic!("ожидали Rejected, получено {other:?}"),
        }
    }
}
