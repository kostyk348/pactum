//! Время: секунды от эпохи и ISO-8601 без внешних крейтов.
//!
//! В Pactum время фигурирует в двух ролях: метки журналов (для людей) и
//! дедлайны контрактов (для машин). Дедлайны — всегда UTC, всегда в ISO-8601.

use std::time::{SystemTime, UNIX_EPOCH};

/// Секунды от эпохи (UTC).
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Миллисекунды от эпохи (UTC).
pub fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// ISO-8601 UTC из секунд от эпохи: `2026-01-31T12:34:56Z`.
pub fn to_iso(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Текущее время в ISO-8601 UTC.
pub fn now_iso() -> String {
    to_iso(now())
}

/// Разобрать `YYYY-MM-DDTHH:MM:SSZ` в секунды от эпохи.
pub fn parse_iso(text: &str) -> Option<u64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return None;
    }
    let y: i64 = text.get(0..4)?.parse().ok()?;
    let mo: u32 = text.get(5..7)?.parse().ok()?;
    let d: u32 = text.get(8..10)?.parse().ok()?;
    let h: u32 = text.get(11..13)?.parse().ok()?;
    let mi: u32 = text.get(14..16)?.parse().ok()?;
    let s: u32 = text.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 59 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    Some((days as u64) * 86_400 + u64::from(h) * 3600 + u64::from(mi) * 60 + u64::from(s))
}

// Алгоритм Говарда Хиннанта: дни <-> гражданская дата.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_roundtrip() {
        assert_eq!(to_iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(to_iso(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(parse_iso("2001-09-09T01:46:40Z"), Some(1_000_000_000));
    }

    #[test]
    fn leap_day() {
        assert!(parse_iso("2024-02-29T12:00:00Z").is_some());
        let secs = parse_iso("2024-02-29T00:00:00Z").unwrap();
        assert_eq!(to_iso(secs), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_iso("не дата"), None);
        assert_eq!(parse_iso("2026-13-40T99:99:99Z"), None);
        assert_eq!(parse_iso("2026-01-01 00:00:00"), None);
    }

    #[test]
    fn now_is_sane() {
        assert!(now() > 1_700_000_000);
    }
}
