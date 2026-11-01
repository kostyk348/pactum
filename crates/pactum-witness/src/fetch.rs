//! Чтение URL для нотаризации: с лимитами, тайм-аутами и защитой от SSRF.
//!
//! Нотариус сам ходит в сеть по просьбе клиента — значит, он должен
//! отказываться ходить туда, куда ходить нельзя. Здесь: только http/https,
//! редиректы вручную с проверкой каждого перехода, приватные и служебные
//! адреса отвергаются до подключения, тело читается с жёстким потолком.
//!
//! Честное ограничение: проверка адреса делается до запроса (по разрешению
//! имени), DNS rebinding между проверкой и подключением остаётся на
//! совести сети — это задокументированный предел, а не секрет.

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use pactum_core::hash;

use crate::error::{WitnessError, WitnessResult};

/// Потолки и правила для чтения URL.
#[derive(Debug, Clone)]
pub struct UrlLimits {
    /// Максимум байтов тела.
    pub max_bytes: usize,
    /// Тайм-аут на соединение и на чтение.
    pub timeout: Duration,
    /// Разрешить приватные/loopback-адреса (для тестов и self-hosted сетей).
    pub allow_private: bool,
    /// Максимум редиректов.
    pub max_redirects: u32,
}

impl Default for UrlLimits {
    fn default() -> UrlLimits {
        UrlLimits {
            max_bytes: 16 * 1024 * 1024,
            timeout: Duration::from_secs(30),
            allow_private: false,
            max_redirects: 5,
        }
    }
}

/// Результат чтения URL.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Итоговый url (после редиректов).
    pub url: String,
    /// Тело ответа.
    pub body: Vec<u8>,
    /// SHA-256 тела — сюда сводится вся нотаризация.
    pub content_sha256: String,
    /// Content-Type источника, если был.
    pub content_type: Option<String>,
    /// HTTP-код финального ответа.
    pub status: u16,
}

/// Прочитать URL и посчитать SHA-256 тела. Каждый переход редиректа
/// проходит ту же проверку адреса, что и первый запрос.
pub fn fetch_url(url: &str, limits: &UrlLimits) -> WitnessResult<Fetched> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(limits.timeout)
        .timeout_read(limits.timeout)
        .build();
    let mut current = url.to_string();
    for hop in 0..=limits.max_redirects {
        check_url(&current, limits)?;
        match agent.get(&current).call() {
            Ok(resp) => {
                let status = resp.status();
                let content_type = resp.header("Content-Type").map(str::to_string);
                let mut body = Vec::new();
                use std::io::Read;
                resp.into_reader()
                    .take(limits.max_bytes as u64 + 1)
                    .read_to_end(&mut body)
                    .map_err(|err| WitnessError::Network(err.to_string()))?;
                if body.len() > limits.max_bytes {
                    return Err(WitnessError::TooLarge {
                        limit: limits.max_bytes,
                    });
                }
                return Ok(Fetched {
                    url: current,
                    content_sha256: hash::sha256_hex(&body),
                    body,
                    content_type,
                    status,
                });
            }
            Err(ureq::Error::Status(status, resp)) if (300..400).contains(&status) => {
                if hop == limits.max_redirects {
                    return Err(WitnessError::TooManyRedirects(limits.max_redirects));
                }
                let location = resp
                    .header("Location")
                    .ok_or_else(|| WitnessError::Upstream {
                        status,
                        detail: "редирект без Location".into(),
                    })?;
                current = join_redirect(&current, location)?;
                continue;
            }
            Err(ureq::Error::Status(status, resp)) => {
                let detail = resp
                    .into_string()
                    .unwrap_or_default()
                    .chars()
                    .take(200)
                    .collect();
                return Err(WitnessError::Upstream { status, detail });
            }
            Err(err) => return Err(WitnessError::Network(err.to_string())),
        }
    }
    unreachable!("цикл выходов выше")
}

/// Схема, хост и адрес запроса: что можно, а что нельзя.
fn check_url(url: &str, limits: &UrlLimits) -> WitnessResult<()> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| WitnessError::BadUrl("нет схемы".into()))?;
    if !matches!(scheme, "http" | "https") {
        return Err(WitnessError::BadUrl(format!(
            "схема {scheme:?} не разрешена"
        )));
    }
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|h| !h.is_empty())
        .ok_or_else(|| WitnessError::BadUrl("нет хоста".into()))?;
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h); // userinfo долой
    let port_split = host.rsplit_once(':');
    let (host, port): (&str, Option<u16>) = match port_split {
        // не спутать IPv6-литерал с портом
        Some((h, p))
            if !h.contains(']') && p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() =>
        {
            (
                h,
                Some(
                    p.parse()
                        .map_err(|_| WitnessError::BadUrl("порт не разобран".into()))?,
                ),
            )
        }
        _ => (host, None),
    };
    let addrs: Vec<SocketAddr> = (
        host,
        port.unwrap_or(if scheme == "https" { 443 } else { 80 }),
    )
        .to_socket_addrs()
        .map_err(|err| WitnessError::BadUrl(format!("хост {host:?} не резолвится: {err}")))?
        .collect();
    let first = addrs
        .first()
        .ok_or_else(|| WitnessError::BadUrl(format!("хост {host:?} не резолвится")))?;
    if !limits.allow_private && is_blocked_ip(first.ip()) {
        return Err(WitnessError::BadUrl(format!(
            "адрес {first} приватный или служебный — нотариус туда не ходит"
        )));
    }
    Ok(())
}

/// Служебные и приватные адреса: loopback, RFC1918, CGNAT, link-local,
/// documentation, unique-local v6, link-local v6.
pub fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || (seg[0] & 0xfe00) == 0xfc00
                || (seg[0] & 0xffc0) == 0xfe80
                || (seg[0] == 0x2001 && seg[1] == 0xdb8)
        }
    }
}

/// Склеить относительный Location с текущим url.
fn join_redirect(current: &str, location: &str) -> WitnessResult<String> {
    if location.contains("://") {
        return Ok(location.to_string());
    }
    let (scheme, rest) = current
        .split_once("://")
        .ok_or_else(|| WitnessError::BadUrl("текущий url без схемы".into()))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if location.starts_with('/') {
        Ok(format!("{scheme}://{authority}{location}"))
    } else {
        Err(WitnessError::BadUrl(format!(
            "относительный редирект без «/» не поддержан: {location:?}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_ip_classes() {
        assert!(is_blocked_ip("127.0.0.1".parse().unwrap()));
        assert!(is_blocked_ip("10.1.2.3".parse().unwrap()));
        assert!(is_blocked_ip("192.168.0.1".parse().unwrap()));
        assert!(is_blocked_ip("172.16.5.5".parse().unwrap()));
        assert!(is_blocked_ip("169.254.1.1".parse().unwrap()));
        assert!(is_blocked_ip("100.64.0.1".parse().unwrap()));
        assert!(is_blocked_ip("0.0.0.0".parse().unwrap()));
        assert!(is_blocked_ip("::1".parse().unwrap()));
        assert!(is_blocked_ip("fd00::1".parse().unwrap()));
        assert!(is_blocked_ip("fe80::1".parse().unwrap()));
        assert!(is_blocked_ip("2001:db8::1".parse().unwrap()));
        assert!(!is_blocked_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_blocked_ip("93.184.216.34".parse().unwrap()));
        assert!(!is_blocked_ip(
            "2606:2800:220:1:248:1893:25c8:1946".parse().unwrap()
        ));
    }

    #[test]
    fn rejects_non_http_schemes() {
        let limits = UrlLimits::default();
        assert!(check_url("file:///etc/passwd", &limits).is_err());
        assert!(check_url("gopher://example.org", &limits).is_err());
        assert!(check_url("example.org/page", &limits).is_err());
    }

    #[test]
    fn rejects_private_hosts() {
        let limits = UrlLimits::default();
        assert!(check_url("http://127.0.0.1/x", &limits).is_err());
        assert!(check_url("http://10.0.0.1/x", &limits).is_err());
        assert!(check_url("http://192.168.1.1/x", &limits).is_err());
        assert!(check_url("http://[::1]/x", &limits).is_err());
    }

    #[test]
    fn allows_private_when_enabled() {
        let limits = UrlLimits {
            allow_private: true,
            ..UrlLimits::default()
        };
        assert!(check_url("http://127.0.0.1:9/x", &limits).is_ok());
    }

    #[test]
    fn rejects_unresolvable_host() {
        let limits = UrlLimits::default();
        assert!(check_url("http://нет-такого-хоста-pactum.invalid/", &limits).is_err());
    }

    #[test]
    fn join_redirect_handles_absolute_and_root() {
        assert_eq!(
            join_redirect("http://a.example/x", "http://b.example/y").unwrap(),
            "http://b.example/y"
        );
        assert_eq!(
            join_redirect("https://a.example/x/y", "/z").unwrap(),
            "https://a.example/z"
        );
        assert!(join_redirect("https://a.example/x", "z").is_err());
    }
}
