//! «Письмо» — универсальный конверт сообщения протокола ACP.
//!
//! Контракт, доказательство, событие потока, квитанция — всё ходит по сети в
//! одном и том же виде: текстовый конверт в духе RFC-822 (заголовки, пустая
//! строка, тело-JSON). Такой конверт читает человек в любом редакторе, его
//! удобно держать в git, его можно переслать чем угодно: HTTP-релеем, файловым
//! спулом, SMTP (адаптер — в дорожной карте), да хоть распечатать.
//!
//! Наследие acp-mvp: служебные заголовки `X-Escrow-Budget`, `X-Timeout-UTC`
//! сохранены для совместимости; `X-Agent-Protocol` переименован в
//! `X-Pactum-Protocol`, потому что участники теперь не только агенты.

use crate::error::{CoreError, CoreResult};
use crate::hash;
use std::collections::BTreeMap;

/// Имя реализации, которым подписываются письма.
pub const PROTOCOL_NAME: &str = "Pactum";

/// Роль письма в обмене.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LetterKind {
    /// Контракт: предложение работы/обязательства.
    Contract,
    /// Доказательство выполнения (ответ исполнителя).
    Proof,
    /// Событие потока (доставка подписчику).
    Event,
    /// Квитанция (подтверждение приёма).
    Receipt,
}

impl LetterKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LetterKind::Contract => "contract",
            LetterKind::Proof => "proof",
            LetterKind::Event => "event",
            LetterKind::Receipt => "receipt",
        }
    }
    pub fn parse(text: &str) -> Option<LetterKind> {
        match text {
            "contract" => Some(LetterKind::Contract),
            "proof" => Some(LetterKind::Proof),
            "event" => Some(LetterKind::Event),
            "receipt" => Some(LetterKind::Receipt),
            _ => None,
        }
    }
}

/// Конверт письма: заголовки + тело.
///
/// Заголовки хранятся в `BTreeMap`: порядок вывода стабилен, повторяющиеся
/// заголовки протоколом не предусмотрены.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Letter {
    pub headers: BTreeMap<String, String>,
    /// Тело письма: JSON-значение (объект контракта, доказательства и т.д.).
    pub body: serde_json::Value,
}

impl Letter {
    pub fn new(kind: LetterKind, from: &str, body: serde_json::Value) -> Letter {
        let mut headers = BTreeMap::new();
        headers.insert("X-Pactum-Protocol".into(), crate::ACP_VERSION.into());
        headers.insert("X-Pactum-Kind".into(), kind.as_str().into());
        headers.insert("X-Pactum-From".into(), from.into());
        headers.insert("Date".into(), crate::time::now_iso());
        Letter { headers, body }
    }

    pub fn kind(&self) -> Option<LetterKind> {
        self.headers
            .get("X-Pactum-Kind")
            .and_then(|v| LetterKind::parse(v))
    }

    pub fn from(&self) -> Option<&str> {
        self.headers.get("X-Pactum-From").map(String::as_str)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers.insert(name.to_string(), value.to_string());
    }

    /// Канонические байты для подписи: отсортированные `Имя: значение\n` + тело.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, value) in &self.headers {
            if name == "X-Pactum-Signature" {
                continue; // подпись не подписывает саму себя
            }
            out.extend_from_slice(format!("{name}: {value}\n").as_bytes());
        }
        out.push(b'\n');
        out.extend_from_slice(self.body.to_string().as_bytes());
        out
    }

    /// Отпечаток письма: SHA-256 канонических байтов без подписи.
    pub fn fingerprint(&self) -> String {
        hash::sha256_hex(&self.signing_bytes())
    }

    /// Сериализация в текстовый конверт (UTF-8).
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (name, value) in &self.headers {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push('\n');
        }
        out.push('\n');
        out.push_str(&serde_json::to_string_pretty(&self.body).unwrap_or_default());
        out.push('\n');
        out
    }

    /// Разбор текстового конверта. Заголовки — до первой пустой строки,
    /// дальше — тело (JSON). Лишние пробелы и \r\n прощаются.
    pub fn parse(text: &str) -> CoreResult<Letter> {
        let normalized = text.replace("\r\n", "\n");
        let mut split = normalized.splitn(2, "\n\n");
        let head = split.next().unwrap_or_default();
        let rest = split.next().unwrap_or_default();
        if head.is_empty() {
            return Err(CoreError::Letter("пустой конверт".into()));
        }
        let mut headers = BTreeMap::new();
        for line in head.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(CoreError::Letter(format!(
                    "заголовок без двоеточия: {line}"
                )));
            };
            let name = name.trim().to_string();
            if name.contains(' ') {
                return Err(CoreError::Letter(format!(
                    "имя заголовка с пробелом: {name}"
                )));
            }
            headers.insert(name, value.trim().to_string());
        }
        if !headers.contains_key("X-Pactum-Protocol") {
            return Err(CoreError::Letter(
                "нет X-Pactum-Protocol — не письмо Pactum".into(),
            ));
        }
        let body_text = rest.trim();
        if body_text.is_empty() {
            return Err(CoreError::Letter("пустое тело письма".into()));
        }
        let body: serde_json::Value = serde_json::from_str(body_text)
            .map_err(|err| CoreError::Letter(format!("тело не JSON: {err}")))?;
        Ok(Letter { headers, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Letter {
        Letter::new(
            LetterKind::Contract,
            "p0123456789abcdef",
            json!({"task": "compile"}),
        )
    }

    #[test]
    fn render_parse_roundtrip() {
        let letter = sample();
        let text = letter.render();
        let back = Letter::parse(&text).expect("разбор");
        assert_eq!(back.kind(), Some(LetterKind::Contract));
        assert_eq!(back.from(), Some("p0123456789abcdef"));
        assert_eq!(back.body, json!({"task": "compile"}));
        assert_eq!(back.fingerprint(), letter.fingerprint());
    }

    #[test]
    fn signature_is_excluded_from_signing_bytes() {
        let mut letter = sample();
        let fp_before = letter.fingerprint();
        letter.set_header("X-Pactum-Signature", "ecdsa-p256:deadbeef");
        assert_eq!(letter.fingerprint(), fp_before);
    }

    #[test]
    fn rejects_non_pactum() {
        assert!(Letter::parse("From: bob\n\nhello").is_err());
        assert!(Letter::parse("X-Pactum-Protocol: acp/1\n\nне-json").is_err());
        assert!(Letter::parse("").is_err());
    }

    #[test]
    fn crlf_is_forgiven() {
        let text = sample().render().replace('\n', "\r\n");
        assert!(Letter::parse(&text).is_ok());
    }

    #[test]
    fn body_is_pretty_json() {
        let text = sample().render();
        assert!(text.contains("\"task\": \"compile\""));
    }
}
