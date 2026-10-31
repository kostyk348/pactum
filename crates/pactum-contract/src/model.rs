//! Модель контракта и её проверка по встроенной схеме.

use pactum_core::error::{CoreError, CoreResult};
use pactum_core::schema;
use serde::{Deserialize, Serialize};

/// Встроенная схема `pactum-contract/v1`. Проверяется до отправки и после
/// получения — плохие контракты не попадают в сеть.
pub const CONTRACT_SCHEMA: &str = include_str!("contract_schema.json");

/// Идентификатор формата контракта.
pub const CONTRACT_FORMAT: &str = "pactum-contract/v1";

/// Откуда исполнителю взять исходные данные работы.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceRef {
    /// inline | git | url | file
    #[serde(rename = "type")]
    pub kind: String,
    /// Для inline — данные (например, base64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// Для git/url/file — ссылка или путь.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Что именно сделать. `type` — capability вида `ci.compile.rust`:
/// произвольный dotted-путь, одинаковый для агентов, сервисов и людей.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Intent {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub domain: String,
    pub title: String,
    pub source: SourceRef,
}

/// Одна проверочная команда. «Проверка должна падать» — тоже законная
/// проверка: задайте ожидаемый код через контракт оффлайн или верьте exit 0.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestCmd {
    pub cmd: String,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
}

fn default_timeout() -> u64 {
    120
}

/// Ожидаемый артефакт: путь (glob `*`, `?`) и минимальный размер.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactExpect {
    pub path: String,
    #[serde(default)]
    pub min_size: u64,
}

/// Как проверять результат. Наследие acp-mvp: команды + артефакты.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Verification {
    pub tests: Vec<TestCmd>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts_expected: Vec<ArtifactExpect>,
}

/// Делёж бюджета: проценты principal и executor (сумма должна быть 100).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoyaltySplit {
    pub principal: i64,
    pub executor: i64,
}

/// Деньги/валюта как строка: никакого float, никакой конкретной валюты.
/// USD, USDC, кредиты организации, минуты машинного времени — что угодно.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Budget {
    pub amount: String,
    pub unit: String,
}

/// Правила расчёта. `provider` — имя SettlementProvider: `sim` для
/// встроенного escrow-движка или адрес смарт-контракта для блокчейна.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Settlement {
    pub provider: String,
    pub budget: Budget,
    pub on_success: String,
    pub on_failure: String,
    #[serde(default = "default_split")]
    pub royalty_split: RoyaltySplit,
    #[serde(default)]
    pub auto_verify: bool,
    /// Сколько процентов ставки исполнителя сгорает при таймауте.
    #[serde(default = "default_slash")]
    pub slash_stake_percent: i64,
}

fn default_split() -> RoyaltySplit {
    RoyaltySplit {
        principal: 90,
        executor: 10,
    }
}

fn default_slash() -> i64 {
    10
}

/// Стороны сделки. Универсальные роли: заказчик (`principal`) и исполнитель
/// (`executor`). Ни та, ни другая роль не обязана быть агентом.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Parties {
    pub principal: String,
    #[serde(default)]
    pub executor: Option<String>,
}

/// Контракт целиком.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Contract {
    pub schema: String,
    pub schema_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<String>,
    pub intent: Intent,
    pub verification: Verification,
    pub settlement: Settlement,
    pub parties: Parties,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_utc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl Contract {
    /// Собрать контракт из полей, присвоив id и nonce.
    pub fn build(
        intent: Intent,
        verification: Verification,
        settlement: Settlement,
        principal: &str,
        deadline_utc: Option<String>,
    ) -> Contract {
        Contract {
            schema: CONTRACT_FORMAT.to_string(),
            schema_version: "1.0.0".to_string(),
            contract_id: Some(pactum_core::id::new_contract_id()),
            nonce: Some(pactum_core::id::random_hex(16)),
            intent,
            verification,
            settlement,
            parties: Parties {
                principal: principal.to_string(),
                executor: None,
            },
            deadline_utc,
            notes: None,
        }
    }

    /// Capability задачи.
    pub fn capability(&self) -> &str {
        &self.intent.kind
    }

    /// Бюджет в читаемом виде.
    pub fn budget_display(&self) -> String {
        format!(
            "{} {}",
            self.settlement.budget.amount, self.settlement.budget.unit
        )
    }

    /// Проверить по встроенной схеме. Ошибки — списком путей.
    pub fn validate(&self) -> CoreResult<()> {
        let value = serde_json::to_value(self)?;
        schema::validate(
            &value,
            &serde_json::from_str(CONTRACT_SCHEMA)
                .map_err(|e| CoreError::Schema(format!("встроенная схема не читается: {e}")))?,
        )?;
        let split = &self.settlement.royalty_split;
        if split.principal + split.executor != 100 {
            return Err(CoreError::Schema(format!(
                "royalty_split: {} + {} ≠ 100",
                split.principal, split.executor
            )));
        }
        // capability ходит в HTTP-заголовках — только ASCII: буквы, цифры, ._-*
        if !self
            .intent
            .kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'*'))
        {
            return Err(CoreError::Schema(
                "intent.type: capability должна быть ASCII (буквы, цифры, . _ - *)".into(),
            ));
        }
        Ok(())
    }

    /// Из JSON-значения с валидацией исходного значения по схеме
    /// (лишние поля ловятся до десериализации, а не выбрасываются serde).
    pub fn from_value(value: &serde_json::Value) -> CoreResult<Contract> {
        let schema_json: serde_json::Value = serde_json::from_str(CONTRACT_SCHEMA)
            .map_err(|e| CoreError::Schema(format!("встроенная схема не читается: {e}")))?;
        schema::validate(value, &schema_json)?;
        let contract: Contract = serde_json::from_value(value.clone())?;
        contract.validate()?;
        Ok(contract)
    }

    /// Из JSON-текста с валидацией.
    pub fn from_json(text: &str) -> CoreResult<Contract> {
        let value: serde_json::Value = serde_json::from_str(text)?;
        Contract::from_value(&value)
    }

    /// В JSON-значение (уже валидированный контракт).
    pub fn to_value(&self) -> CoreResult<serde_json::Value> {
        self.validate()?;
        Ok(serde_json::to_value(self)?)
    }

    /// Дедлайн в секундах от эпохи, если задан.
    pub fn deadline_secs(&self) -> Option<u64> {
        self.deadline_utc
            .as_deref()
            .and_then(pactum_core::time::parse_iso)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Contract {
        Contract {
            schema: CONTRACT_FORMAT.into(),
            schema_version: "1.0.0".into(),
            contract_id: Some("c-test0001".into()),
            nonce: Some("n-123".into()),
            intent: Intent {
                kind: "ci.compile.rust".into(),
                domain: "rust".into(),
                title: "Собрать wasm".into(),
                source: SourceRef {
                    kind: "inline".into(),
                    data: Some("echo ok".into()),
                    reference: None,
                },
            },
            verification: Verification {
                tests: vec![TestCmd {
                    cmd: "echo ok".into(),
                    timeout_s: 30,
                }],
                artifacts_expected: vec![],
            },
            settlement: Settlement {
                provider: "sim".into(),
                budget: Budget {
                    amount: "10.00".into(),
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
            parties: Parties {
                principal: "p0123456789abcdef".into(),
                executor: None,
            },
            deadline_utc: Some("2026-12-31T23:59:59Z".into()),
            notes: None,
        }
    }

    #[test]
    fn valid_contract_passes() {
        assert!(sample().validate().is_ok());
    }

    #[test]
    fn royalty_must_sum_to_hundred() {
        let mut c = sample();
        c.settlement.royalty_split = RoyaltySplit {
            principal: 50,
            executor: 10,
        };
        let err = c.validate().unwrap_err().to_string();
        assert!(err.contains("≠ 100"), "{err}");
    }

    #[test]
    fn capability_must_be_dotted() {
        let mut c = sample();
        c.intent.kind = "простая-строка".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn deadline_parses() {
        let secs = sample().deadline_secs();
        assert_eq!(secs, Some(1_798_761_599), "2026-12-31T23:59:59Z");
        assert_eq!(
            pactum_core::time::to_iso(secs.unwrap()),
            "2026-12-31T23:59:59Z"
        );
    }

    #[test]
    fn json_roundtrip_with_validation() {
        let c = sample();
        let json_text = serde_json::to_string_pretty(&c).unwrap();
        let back = Contract::from_json(&json_text).expect("валидация");
        assert_eq!(back, c);
    }

    #[test]
    fn rejects_unknown_fields() {
        let mut value = serde_json::to_value(sample()).unwrap();
        value["внезапно"] = serde_json::json!(1);
        assert!(Contract::from_value(&value).is_err());
    }

    #[test]
    fn build_assigns_ids() {
        let c = Contract::build(
            sample().intent,
            sample().verification,
            sample().settlement,
            "p0123456789abcdef",
            None,
        );
        assert!(c.contract_id.as_deref().unwrap_or("").starts_with("c-"));
        assert!(c.validate().is_ok());
    }
}
