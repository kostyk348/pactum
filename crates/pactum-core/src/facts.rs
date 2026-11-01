//! Цепочка фактов: память узла, которую нельзя подделать.
//!
//! Наследник `sint_ua` из acp-mvp. Каждый факт — блок с SHA-256, содержащим
//! хеш предыдущего блока. Любое изменение истории ломает цепочку — это
//! проверяется за O(n) одной командой.
//!
//! Регистры универсальны и не привязаны к агентам — это эпистемология
//! любого участника: человек, сервис или датчик одинаково наблюдают,
//! фиксируют, делают выводы, ошибаются мнениями и действуют.

use crate::error::{CoreError, CoreResult};
use crate::hash;
use serde::{Deserialize, Serialize};

/// Эпистемический регистр факта.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Register {
    /// Наблюдение: «вижу, что выходные байты изменились».
    Sense,
    /// Проверенный факт: «сборка прошла, exit 0».
    Fact,
    /// Вывод: «значит, исходник правили после релиза».
    Logic,
    /// Мнение (может оказаться неверным, и это честно помечено).
    Opinion,
    /// Действие: «опубликовал контракт c-...».
    Action,
}

impl Register {
    pub fn parse(text: &str) -> Option<Register> {
        match text.to_uppercase().as_str() {
            "SENSE" => Some(Register::Sense),
            "FACT" => Some(Register::Fact),
            "LOGIC" => Some(Register::Logic),
            "OPINION" => Some(Register::Opinion),
            "ACTION" => Some(Register::Action),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Register::Sense => "SENSE",
            Register::Fact => "FACT",
            Register::Logic => "LOGIC",
            Register::Opinion => "OPINION",
            Register::Action => "ACTION",
        }
    }
}

/// Один блок цепочки.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Fact {
    pub seq: u64,
    /// ISO-8601 UTC момент записи.
    pub ts: String,
    pub register: Register,
    /// Текст факта: что именно произошло.
    pub text: String,
    /// Привязка к контракту, если факт о сделке.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contract_id: Option<String>,
    /// Хеш предыдущего блока (пусто для генезиса).
    pub prev_hash: String,
    /// Хеш этого блока.
    pub hash: String,
}

impl Fact {
    /// Вычислить хеш блока по содержимому (без поля `hash`).
    pub fn content_hash(&self) -> String {
        let payload = format!(
            "{}|{}|{}|{}|{}|{}",
            self.seq,
            self.ts,
            self.register.as_str(),
            self.text,
            self.contract_id.as_deref().unwrap_or(""),
            self.prev_hash
        );
        hash::sha256_text(&payload)
    }
}

/// Цепочка фактов узла. Хранится как JSON-файл (или в памяти).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FactChain {
    pub facts: Vec<Fact>,
}

impl FactChain {
    pub fn new() -> FactChain {
        FactChain::default()
    }

    /// Добавить факт: хеши считаются здесь, вручную подсовывать нельзя.
    pub fn append(&mut self, register: Register, text: &str, contract_id: Option<&str>) -> &Fact {
        let prev_hash = self
            .facts
            .last()
            .map(|f| f.hash.clone())
            .unwrap_or_default();
        let mut fact = Fact {
            seq: self.facts.len() as u64 + 1,
            ts: crate::time::now_iso(),
            register,
            text: text.to_string(),
            contract_id: contract_id.map(str::to_string),
            prev_hash,
            hash: String::new(),
        };
        fact.hash = fact.content_hash();
        self.facts.push(fact);
        self.facts.last().expect("только что добавили")
    }

    /// Проверить всю цепочку за O(n). Возвращает Err с номером блока.
    pub fn verify(&self) -> CoreResult<()> {
        let mut prev = String::new();
        for fact in &self.facts {
            if fact.seq != prev_len_plus_one(&self.facts, fact) {
                return Err(CoreError::ChainBroken(
                    fact.seq,
                    "нарушена нумерация".into(),
                ));
            }
            if fact.prev_hash != prev {
                return Err(CoreError::ChainBroken(
                    fact.seq,
                    "хеш предыдущего блока не сходится".into(),
                ));
            }
            if fact.hash != fact.content_hash() {
                return Err(CoreError::ChainBroken(
                    fact.seq,
                    "содержимое подменено".into(),
                ));
            }
            prev = fact.hash.clone();
        }
        Ok(())
    }

    /// Фильтр по регистру и/или контракту.
    pub fn filter(&self, register: Option<Register>, contract_id: Option<&str>) -> Vec<&Fact> {
        self.facts
            .iter()
            .filter(|f| register.map(|r| f.register == r).unwrap_or(true))
            .filter(|f| {
                contract_id
                    .map(|c| f.contract_id.as_deref() == Some(c))
                    .unwrap_or(true)
            })
            .collect()
    }

    /// Сохранить в файл (JSON). Родительские папки создаёт сама.
    pub fn save(&self, path: &std::path::Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Загрузить из файла, сразу проверяя цепочку.
    pub fn load(path: &std::path::Path) -> CoreResult<FactChain> {
        let text = std::fs::read_to_string(path)?;
        let chain: FactChain = serde_json::from_str(&text)?;
        chain.verify()?;
        Ok(chain)
    }
}

fn prev_len_plus_one(facts: &[Fact], current: &Fact) -> u64 {
    // seq должен быть равен позиции блока + 1
    (facts
        .iter()
        .position(|f| f.hash == current.hash)
        .unwrap_or(0) as u64)
        + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_appends_and_verifies() {
        let mut chain = FactChain::new();
        chain.append(Register::Action, "опубликовал контракт", Some("c-1"));
        chain.append(Register::Fact, "исполнение вернуло exit 0", Some("c-1"));
        chain.append(Register::Opinion, "исполнителю можно доверять", None);
        assert_eq!(chain.facts.len(), 3);
        assert!(chain.verify().is_ok());
    }

    #[test]
    fn tampering_breaks_chain() {
        let mut chain = FactChain::new();
        chain.append(Register::Fact, "тестов 80", None);
        chain.append(Register::Fact, "тестов зелёные", None);
        chain.facts[0].text = "тестов 20".to_string();
        let err = chain.verify().unwrap_err();
        match err {
            CoreError::ChainBroken(seq, _) => assert_eq!(seq, 1),
            other => panic!("не та ошибка: {other}"),
        }
    }

    #[test]
    fn renumbering_breaks_chain() {
        let mut chain = FactChain::new();
        chain.append(Register::Sense, "наблюдение", None);
        chain.append(Register::Sense, "ещё наблюдение", None);
        chain.facts[1].seq = 7;
        assert!(chain.verify().is_err());
    }

    #[test]
    fn filter_by_register_and_contract() {
        let mut chain = FactChain::new();
        chain.append(Register::Action, "a", Some("c-1"));
        chain.append(Register::Fact, "f", Some("c-1"));
        chain.append(Register::Fact, "g", None);
        assert_eq!(chain.filter(Some(Register::Fact), None).len(), 2);
        assert_eq!(chain.filter(Some(Register::Fact), Some("c-1")).len(), 1);
        assert_eq!(chain.filter(None, Some("c-1")).len(), 2);
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pactum-facts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("facts.json");
        let mut chain = FactChain::new();
        chain.append(Register::Logic, "вывод", None);
        chain.save(&path).unwrap();
        let loaded = FactChain::load(&path).unwrap();
        assert_eq!(loaded.facts.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
