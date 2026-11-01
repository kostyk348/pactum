//! Ограничитель частоты: фиксированное окно на IP-адрес клиента.
//!
//! Нотариус — публичная услуга, поэтому безлимитный приём нечестен по
//! отношению к самому себе: окно простое, предсказуемое и честно отвечает
//! `429` вместо тихой деградации.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

/// Окно по умолчанию — 60 секунд.
pub const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

/// Фиксированное окно на IP: не больше `max` запросов за окно.
#[derive(Debug)]
pub struct RateLimiter {
    window_ms: u64,
    max: u32,
    buckets: HashMap<IpAddr, (u64, u32)>,
}

impl RateLimiter {
    /// Лимитер с окном 60 с и лимитом `per_window` запросов на IP.
    pub fn new(per_window: u32) -> RateLimiter {
        RateLimiter {
            window_ms: DEFAULT_WINDOW.as_millis() as u64,
            max: per_window,
            buckets: HashMap::new(),
        }
    }

    /// Пропустить запрос? `now_ms` — текущее время в миллисекундах.
    pub fn check(&mut self, ip: IpAddr, now_ms: u64) -> bool {
        if self.max == 0 {
            return false;
        }
        let window_ms = self.window_ms;
        let max = self.max;
        let bucket = self.buckets.entry(ip).or_insert((now_ms, 0));
        if now_ms.saturating_sub(bucket.0) >= window_ms {
            *bucket = (now_ms, 0);
        }
        if bucket.1 >= max {
            return false;
        }
        bucket.1 += 1;
        true
    }

    /// Сколько IP сейчас в учёте (для наблюдаемости).
    pub fn tracked(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, a))
    }

    #[test]
    fn allows_up_to_limit_then_429() {
        let mut limiter = RateLimiter::new(3);
        assert!(limiter.check(ip(1), 1_000));
        assert!(limiter.check(ip(1), 1_001));
        assert!(limiter.check(ip(1), 1_002));
        assert!(!limiter.check(ip(1), 1_003), "четвёртый в окне — отказ");
    }

    #[test]
    fn window_reset_restores_access() {
        let mut limiter = RateLimiter::new(1);
        assert!(limiter.check(ip(2), 5_000));
        assert!(!limiter.check(ip(2), 5_100));
        assert!(
            limiter.check(ip(2), 5_000 + 60_000),
            "новое окно — доступ вернулся"
        );
    }

    #[test]
    fn addresses_are_independent() {
        let mut limiter = RateLimiter::new(1);
        assert!(limiter.check(ip(3), 1_000));
        assert!(limiter.check(ip(4), 1_000));
        assert!(!limiter.check(ip(3), 1_000));
    }

    #[test]
    fn ipv6_keys_are_separate() {
        let mut limiter = RateLimiter::new(1);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert!(limiter.check(v6, 1_000));
        assert!(!limiter.check(v6, 1_000));
    }

    #[test]
    fn zero_limit_denies_everything() {
        let mut limiter = RateLimiter::new(0);
        assert!(!limiter.check(ip(5), 1_000));
    }
}
