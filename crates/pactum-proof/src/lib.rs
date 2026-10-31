//! # pactum-proof
//!
//! Заявление с доказательством, которое можно перепроверить.
//!
//! Порт `proof` (kostyk348/proof) в состав Pactum. Проблема та же, у каких
//! бы участников ни был обмен — у агентов, людей или сервисов: «работает»
//! означает «я запустил и вот вывод», а через день это утверждение нельзя
//! проверить — вывод остался в переписке, команда забыта, репозиторий уехал.
//! Здесь заявление хранится вместе с **точной командой** и **отпечатком
//! вывода**, и любой следующий участник перепроверяет его одной командой —
//! не веря на слово.
//!
//! Три вердикта, и они честные:
//!
//! * [`Status::Verified`] — команда вернула ожидаемый код, вывод тот же;
//! * [`Status::Moved`] — команда работает, но вывод изменился (тестов стало
//!   82 вместо 80): заявление устарело, а не сломалось;
//! * [`Status::Broken`] — команда вернула не тот код: доказательство
//!   перестало быть доказательством.
//!
//! Отпечаток считается по **нормализованному** выводу: числа-тайминги
//! (`0.12s`) заменяются на `<t>`, иначе любой прогон тестов давал бы
//! «moved» из-за миллисекунд.
//!
//! Отличие от оригинала: у команды есть тайм-аут (`--timeout`, по умолчанию
//! 120 c) — зависшая проверка не подвешивает `check`, а честно ругается.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use thiserror::Error;

/// Тайм-аут проверочной команды по умолчанию.
pub const DEFAULT_TIMEOUT_S: u64 = 120;

#[derive(Debug, Error)]
pub enum ProofError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("команда «{cmd}» не запустилась: {err}")]
    Spawn { cmd: String, err: String },
    #[error(
        "команда «{cmd}» не уложилась в {timeout_s} с — проверка честно оборвана, а не подвешена"
    )]
    Timeout { cmd: String, timeout_s: u64 },
    #[error("заявление #{0} не найдено")]
    NotFound(u32),
}

pub type ProofResult<T> = Result<T, ProofError>;

/// Вердикт по заявлению.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Ещё не запускали.
    Never,
    /// Верно и подтверждено.
    Verified,
    /// Работает, но вывод изменился — заявление устарело.
    Moved,
    /// Команда больше не возвращает ожидаемое.
    Broken,
}

impl Status {
    pub fn as_str(&self) -> &'static str {
        match self {
            Status::Never => "never",
            Status::Verified => "verified",
            Status::Moved => "moved",
            Status::Broken => "broken",
        }
    }
    pub fn parse(text: &str) -> Status {
        match text {
            "verified" => Status::Verified,
            "moved" => Status::Moved,
            "broken" => Status::Broken,
            _ => Status::Never,
        }
    }
    /// Человеческое имя вердикта (русское — как в оригинале).
    pub fn ru(&self) -> &'static str {
        match self {
            Status::Never => "не проверялось",
            Status::Verified => "подтверждено",
            Status::Moved => "устарело",
            Status::Broken => "сломано",
        }
    }
}

/// Заявление: фраза + команда + отпечаток.
#[derive(Debug, Clone, Serialize)]
pub struct Claim {
    pub id: u32,
    /// Что утверждается: «80 тестов зелёные».
    pub claim: String,
    /// Чем это проверяется: точная команда.
    pub cmd: String,
    /// Где запускать.
    pub dir: String,
    /// Кто заявил.
    pub by: String,
    /// Ожидаемый код возврата («проверка должна падать» — тоже доказательство).
    pub expect: i32,
    /// Тайм-аут команды, секунды.
    pub timeout_s: u64,
    /// Когда заявлено (сек. от эпохи).
    pub made: u64,
    pub runs: u32,
    pub status: Status,
    pub last_run: u64,
    /// Отпечаток нормализованного вывода.
    pub digest: String,
    /// Хвост вывода (3 строки) — для людей.
    pub excerpt: String,
}

impl Claim {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u32,
        claim: &str,
        cmd: &str,
        dir: &str,
        by: &str,
        expect: i32,
        timeout_s: u64,
        made: u64,
    ) -> Claim {
        Claim {
            id,
            claim: claim.to_string(),
            cmd: cmd.to_string(),
            dir: dir.to_string(),
            by: by.to_string(),
            expect,
            timeout_s,
            made,
            runs: 0,
            status: Status::Never,
            last_run: 0,
            digest: String::new(),
            excerpt: String::new(),
        }
    }

    /// Плоский текстовый формат реестра: один файл на заявление,
    /// git-дружелюбный, читается в любом редакторе.
    pub fn render(&self) -> String {
        format!(
            "id={}\nclaim={}\ncmd={}\ndir={}\nby={}\nexpect={}\ntimeout={}\nmade={}\nruns={}\nstatus={}\nlast_run={}\ndigest={}\nexcerpt={}\n",
            self.id,
            escape(&self.claim),
            escape(&self.cmd),
            escape(&self.dir),
            escape(&self.by),
            self.expect,
            self.timeout_s,
            self.made,
            self.runs,
            self.status.as_str(),
            self.last_run,
            self.digest,
            escape(&self.excerpt)
        )
    }

    pub fn parse(text: &str) -> Option<Claim> {
        let mut claim = Claim::new(0, "", "", "", "", 0, DEFAULT_TIMEOUT_S, 0);
        let mut seen_id = false;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = unescape(value.trim());
            match key.trim() {
                "id" => {
                    claim.id = value.parse().unwrap_or(0);
                    seen_id = true;
                }
                "claim" => claim.claim = value,
                "cmd" => claim.cmd = value,
                "dir" => claim.dir = value,
                "by" => claim.by = value,
                "expect" => claim.expect = value.parse().unwrap_or(0),
                "timeout" => claim.timeout_s = value.parse().unwrap_or(DEFAULT_TIMEOUT_S),
                "made" => claim.made = value.parse().unwrap_or(0),
                "runs" => claim.runs = value.parse().unwrap_or(0),
                "status" => claim.status = Status::parse(&value),
                "last_run" => claim.last_run = value.parse().unwrap_or(0),
                "digest" => claim.digest = value,
                "excerpt" => claim.excerpt = value,
                _ => {}
            }
        }
        if !seen_id || claim.id == 0 || claim.claim.is_empty() {
            return None;
        }
        Some(claim)
    }
}

fn escape(text: &str) -> String {
    text.replace('\n', "⏎")
}

fn unescape(text: &str) -> String {
    text.replace("⏎", "\n")
}

/// Нормализация: убрать из вывода то, что меняется от прогона к прогону
/// и ничего не значит (тайминги). `0.012s`, `1.38s`, `12s` → `<t>`.
pub fn normalize(output: &str) -> String {
    let mut out = String::with_capacity(output.len());
    for line in output.lines() {
        let mut first = true;
        for token in line.split_whitespace() {
            if !first {
                out.push(' ');
            }
            first = false;
            out.push_str(&stable_token(token));
        }
        out.push('\n');
    }
    out
}

fn stable_token(token: &str) -> String {
    let trimmed = token.trim_end_matches([',', ';', ')', ':']);
    let suffix = &token[trimmed.len()..];
    if let Some(number) = trimmed.strip_suffix('s') {
        if !number.is_empty() && number.parse::<f64>().is_ok() {
            return format!("<t>{suffix}");
        }
    }
    token.to_string()
}

/// Результат прогона.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub code: i32,
    /// Отпечаток нормализованного вывода (FNV-1a 64: сравнение, не криптография).
    pub digest: String,
    /// Хвост нормализованного вывода — 3 строки.
    pub excerpt: String,
    pub normalized: String,
}

/// Запустить команду заявления с тайм-аутом. Единственное место,
/// где что-то исполняется. Команда выполняется через `sh -c` на unix
/// и напрямую на других платформах.
pub fn run(claim: &Claim) -> ProofResult<RunResult> {
    run_with_timeout(claim, Duration::from_secs(claim.timeout_s.max(1)))
}

/// Как [`run`], но с явным тайм-аутом (для тестов).
pub fn run_with_timeout(claim: &Claim, timeout: Duration) -> ProofResult<RunResult> {
    let mut command = build_command(&claim.cmd, &claim.dir);
    // Порядок: сначала опрос с тайм-аутом, потом чтение пайпов.
    // (spawn → try_wait цикл → чтение вывода после выхода)
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| ProofError::Spawn {
            cmd: claim.cmd.clone(),
            err: err.to_string(),
        })?;

    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break Err(ProofError::Timeout {
                cmd: claim.cmd.clone(),
                timeout_s: timeout.as_secs(),
            });
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let status = status?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        io::Read::read_to_end(&mut pipe, &mut stdout)?;
    }
    if let Some(mut pipe) = child.stderr.take() {
        io::Read::read_to_end(&mut pipe, &mut stderr)?;
    }
    let mut text = String::from_utf8_lossy(&stdout).to_string();
    let errors = String::from_utf8_lossy(&stderr).to_string();
    if !errors.trim().is_empty() {
        text.push_str("\n--- stderr ---\n");
        text.push_str(&errors);
    }
    let normalized = normalize(&text);
    let tail: Vec<&str> = normalized.lines().rev().take(3).collect();
    let excerpt = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    Ok(RunResult {
        code: status.code().unwrap_or(-1),
        digest: pactum_core::hash::fnv1a64(&normalized),
        excerpt,
        normalized,
    })
}

#[cfg(unix)]
fn build_command(cmd: &str, dir: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    if !dir.is_empty() {
        command.current_dir(dir);
    }
    command
}

#[cfg(not(unix))]
fn build_command(cmd: &str, dir: &str) -> Command {
    let mut command = Command::new("cmd");
    command.arg("/C").arg(cmd);
    if !dir.is_empty() {
        command.current_dir(dir);
    }
    command
}

/// Вердикт: код совпал — работает; вывод тот же — подтверждено.
pub fn judge(claim: &Claim, result: &RunResult) -> Status {
    if result.code != claim.expect {
        return Status::Broken;
    }
    if claim.digest.is_empty() || claim.digest == result.digest {
        Status::Verified
    } else {
        Status::Moved
    }
}

/// Применить прогон к заявлению, сохранив историю.
pub fn apply(claim: &mut Claim, result: &RunResult, at: u64) {
    claim.status = judge(claim, result);
    claim.runs += 1;
    claim.last_run = at;
    claim.digest = result.digest.clone();
    claim.excerpt = result.excerpt.clone();
}

/// Прогнать и записать в заявление одной операцией.
pub fn check_now(claim: &mut Claim) -> ProofResult<Status> {
    let result = run(claim)?;
    apply(claim, &result, pactum_core::time::now());
    Ok(claim.status)
}

/// Реестр заявлений — каталог `proofs/` с одним файлом на заявление.
/// Плоские текстовые файлы: можно держать в git и читать глазами.
pub struct Ledger {
    root: PathBuf,
}

impl Ledger {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Ledger> {
        fs::create_dir_all(path.as_ref())?;
        Ok(Ledger {
            root: path.as_ref().to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn claims(&self) -> Vec<Claim> {
        let mut out = Vec::new();
        let Ok(entries) = fs::read_dir(&self.root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("proof") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Some(claim) = Claim::parse(&text) {
                out.push(claim);
            }
        }
        out.sort_by_key(|claim| claim.id);
        out
    }

    pub fn next_id(&self) -> u32 {
        self.claims()
            .iter()
            .map(|claim| claim.id)
            .max()
            .unwrap_or(0)
            + 1
    }

    pub fn put(&self, claim: &Claim) -> io::Result<()> {
        fs::write(
            self.root.join(format!("{:04}.proof", claim.id)),
            claim.render(),
        )
    }

    pub fn get(&self, id: u32) -> Option<Claim> {
        self.claims().into_iter().find(|claim| claim.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(cmd: &str) -> Claim {
        Claim::new(1, "тест", cmd, "", "tester", 0, 10, 0)
    }

    #[test]
    fn normalize_timings() {
        assert_eq!(normalize("Ran 80 tests in 0.01s"), "Ran 80 tests in <t>\n");
        assert_eq!(normalize("done in 1.38s; ok"), "done in <t>; ok\n");
        assert_eq!(normalize("12s total"), "<t> total\n");
        assert_eq!(normalize("80 tests"), "80 tests\n");
    }

    #[test]
    fn same_fingerprint_despite_timing() {
        let a = normalize("Ran 80 tests in 0.01s\nOK (80 tests)");
        let b = normalize("Ran 80 tests in 1.4s\nOK (80 tests)");
        assert_eq!(
            pactum_core::hash::fnv1a64(&a),
            pactum_core::hash::fnv1a64(&b)
        );
        let c = normalize("Ran 82 tests in 0.01s\nOK (82 tests)");
        assert_ne!(
            pactum_core::hash::fnv1a64(&a),
            pactum_core::hash::fnv1a64(&c)
        );
    }

    #[test]
    fn run_and_verdicts() {
        let mut c = claim("echo hello world");
        let r = run(&c).expect("запуск");
        assert_eq!(r.code, 0);
        apply(&mut c, &r, 1);
        assert_eq!(c.status, Status::Verified);

        // вывод изменился — устарело, не сломано
        let mut c2 = c.clone();
        let r2 = run_with_timeout(&c2, Duration::from_secs(5)).unwrap();
        c2.digest = "старый отпечаток".into();
        assert_eq!(judge(&c2, &r2), Status::Moved);

        // код не тот — сломано
        let mut c3 = claim("false");
        c3.expect = 0;
        let r3 = run(&c3).unwrap();
        assert_eq!(judge(&c3, &r3), Status::Broken);
    }

    #[test]
    fn expect_nonzero_is_legal() {
        let mut c = claim("sh -c 'exit 3'");
        c.expect = 3;
        let r = run(&c).unwrap();
        assert_eq!(r.code, 3);
        apply(&mut c, &r, 1);
        assert_eq!(c.status, Status::Verified);
    }

    #[test]
    fn hanging_command_is_cut_by_timeout() {
        let c = claim("sleep 30");
        let started = std::time::Instant::now();
        let result = run_with_timeout(&c, Duration::from_millis(300));
        assert!(matches!(result, Err(ProofError::Timeout { .. })));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn ledger_roundtrip_and_multiline_claim() {
        let dir = std::env::temp_dir().join(format!("pactum-proof-{}", std::process::id()));
        let ledger = Ledger::open(&dir).unwrap();
        let mut c = Claim::new(
            ledger.next_id(),
            "строка первая\nстрока вторая",
            "echo ok",
            "",
            "tester",
            0,
            10,
            pactum_core::time::now(),
        );
        let r = run(&c).unwrap();
        apply(&mut c, &r, pactum_core::time::now());
        ledger.put(&c).unwrap();
        let back = ledger.get(c.id).unwrap();
        assert_eq!(back.claim, "строка первая\nстрока вторая");
        assert_eq!(back.status, Status::Verified);
        assert_eq!(back.digest, c.digest);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn excerpt_is_three_lines() {
        let c = claim("printf 'a\\nb\\nc\\nd\\n'");
        let r = run(&c).unwrap();
        assert_eq!(r.excerpt.lines().count(), 3);
        assert!(r.excerpt.ends_with('d'));
    }
}
