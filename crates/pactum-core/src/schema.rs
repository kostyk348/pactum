//! Валидатор JSON по схеме: рабочее подмножество JSON Schema Draft-07.
//!
//! Плохие контракты не должны попадать в сеть (урок acp-mvp). Полный Draft-07
//! сюда тащить незачем — Pactum использует строгое подмножество и ровно его
//! проверяет:
//!
//! * `type` (string|integer|number|boolean|object|array|null);
//! * `properties`, `required`, `additionalProperties` (bool или схема);
//! * `items`, `minItems`, `maxItems`;
//! * `enum`, `minimum`, `maximum`, `minLength`, `maxLength`;
//! * `pattern` — подмножество: `*`, `?` и литералы (glob, а не regex);
//! * `patternProperties` — то же glob-подмножество.
//!
//! Честное ограничение: это не regex. Для полей контракта хватает glob'ов.

use crate::error::{CoreError, CoreResult};
use serde_json::Value;

/// Валидировать `value` по схеме. Ошибки накапливаются списком путей.
pub fn validate(value: &Value, schema: &Value) -> CoreResult<()> {
    let mut errors = Vec::new();
    check(value, schema, "$", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(CoreError::Schema(errors.join("; ")))
    }
}

fn check(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(obj) = schema.as_object() else {
        return; // не схема — не проверяем
    };

    if let Some(types) = obj.get("type").and_then(Value::as_str) {
        if !type_matches(value, types) {
            errors.push(format!(
                "{path}: ожидался тип {types}, получен {}",
                type_name(value)
            ));
            return;
        }
    }

    if let Some(enum_values) = obj.get("enum").and_then(Value::as_array) {
        if !enum_values.contains(value) {
            errors.push(format!("{path}: значение вне enum"));
        }
    }

    if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
        if let Some(num) = value.as_f64() {
            if num < min {
                errors.push(format!("{path}: {num} меньше минимума {min}"));
            }
        }
    }
    if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
        if let Some(num) = value.as_f64() {
            if num > max {
                errors.push(format!("{path}: {num} больше максимума {max}"));
            }
        }
    }
    if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
        if let Some(text) = value.as_str() {
            if (text.chars().count() as u64) < min {
                errors.push(format!("{path}: строка короче {min}"));
            }
        }
    }
    if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
        if let Some(text) = value.as_str() {
            if (text.chars().count() as u64) > max {
                errors.push(format!("{path}: строка длиннее {max}"));
            }
        }
    }

    if let Some(pattern) = obj.get("pattern").and_then(Value::as_str) {
        if let Some(text) = value.as_str() {
            if !glob_match(pattern, text) {
                errors.push(format!("{path}: «{text}» не подходит под «{pattern}»"));
            }
        }
    }

    match value {
        Value::Object(map) => {
            for req in obj
                .get("required")
                .and_then(Value::as_array)
                .unwrap_or(&vec![])
            {
                if let Some(key) = req.as_str() {
                    if !map.contains_key(key) {
                        errors.push(format!("{path}.{key}: обязателен, отсутствует"));
                    }
                }
            }
            let props = obj.get("properties").and_then(Value::as_object);
            if let Some(props) = props {
                for (key, sub) in props {
                    if let Some(item) = map.get(key) {
                        check(item, sub, &format!("{path}.{key}"), errors);
                    }
                }
            }
            match obj.get("additionalProperties") {
                None | Some(Value::Bool(true)) => {}
                Some(Value::Bool(false)) => {
                    for key in map.keys() {
                        let known = props.map(|p| p.contains_key(key)).unwrap_or(false);
                        let pat = obj.get("patternProperties").and_then(Value::as_object);
                        let matched = pat
                            .map(|p| p.keys().any(|pat| glob_match(pat, key)))
                            .unwrap_or(false);
                        if !known && !matched {
                            errors.push(format!("{path}.{key}: лишнее поле"));
                        }
                    }
                }
                Some(sub) => {
                    for (key, item) in map {
                        let known = props.map(|p| p.contains_key(key)).unwrap_or(false);
                        if !known {
                            check(item, sub, &format!("{path}.{key}"), errors);
                        }
                    }
                }
            }
            if let Some(pats) = obj.get("patternProperties").and_then(Value::as_object) {
                for (pat, sub) in pats {
                    for (key, item) in map {
                        if glob_match(pat, key) {
                            check(item, sub, &format!("{path}.{key}"), errors);
                        }
                    }
                }
            }
        }
        Value::Array(items) => {
            let n = items.len() as u64;
            if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
                if n < min {
                    errors.push(format!("{path}: массив короче {min}"));
                }
            }
            if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
                if n > max {
                    errors.push(format!("{path}: массив длиннее {max}"));
                }
            }
            if let Some(sub) = obj.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(item, sub, &format!("{path}[{i}]"), errors);
                }
            }
        }
        _ => {}
    }
}

fn type_matches(value: &Value, types: &str) -> bool {
    // допускаем и массив типов: ["object", "null"]
    types
        .split(',')
        .map(str::trim)
        .any(|t| type_matches_one(value, t))
}

fn type_matches_one(value: &Value, t: &str) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Glob-сопоставление: `*` — любые символы (включая `/`), `?` — один символ.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_inner(pattern.as_bytes(), text.as_bytes())
}

fn glob_inner(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            glob_inner(&pat[1..], text) || (!text.is_empty() && glob_inner(pat, &text[1..]))
        }
        (Some(b'?'), Some(_)) => glob_inner(&pat[1..], &text[1..]),
        (Some(p), Some(t)) => p == t && glob_inner(&pat[1..], &text[1..]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_valid_contract_shape() {
        let schema = json!({
            "type": "object",
            "required": ["schema", "intent"],
            "additionalProperties": false,
            "properties": {
                "schema": {"type": "string", "enum": ["pactum-contract/v1"]},
                "intent": {
                    "type": "object",
                    "required": ["type"],
                    "properties": {"type": {"type": "string", "pattern": "*.*"}}
                }
            }
        });
        let good = json!({"schema": "pactum-contract/v1", "intent": {"type": "ci.compile"}});
        assert!(validate(&good, &schema).is_ok());
        let bad = json!({"schema": "pactum-contract/v2", "intent": {}, "extra": 1});
        let err = validate(&bad, &schema).unwrap_err().to_string();
        assert!(err.contains("вне enum"));
        assert!(err.contains("лишнее"));
        assert!(err.contains("обязателен"));
    }

    #[test]
    fn glob_semantics() {
        assert!(glob_match("ci.compile*", "ci.compile.rust"));
        assert!(glob_match("*.wasm", "target/box.wasm"));
        assert!(!glob_match("ci.compile", "ci.compile.rust"));
        assert!(glob_match("a?c", "abc"));
    }

    #[test]
    fn arrays_and_numbers() {
        let schema =
            json!({"type": "array", "minItems": 1, "items": {"type": "integer", "minimum": 0}});
        assert!(validate(&json!([0, 1]), &schema).is_ok());
        let err = validate(&json!([-1]), &schema).unwrap_err().to_string();
        assert!(err.contains("минимума"));
    }
}
