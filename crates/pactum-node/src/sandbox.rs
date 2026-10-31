//! Песочница: исполнение проверочных команд контракта и сбор доказательства.
//!
//! Честное ограничение v1.0: это локальный исполнитель с тайм-аутами, а не
//! TEE. Наследие acp-mvp требует большего — docker с жёсткими лимитами
//! (network=none, read-only, cap-drop ALL), затем TEE/ZK. Здесь заложен
//! интерфейс [`Sandbox`]: локальная реализация — [`LocalSandbox`],
//! docker/TEE-реализации подставляются без изменения протокола.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use pactum_contract::{ArtifactExpect, Contract, Verification};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("команда «{cmd}» не запустилась: {err}")]
    Spawn { cmd: String, err: String },
    #[error("команда «{cmd}» не уложилась в {timeout_s} с")]
    Timeout { cmd: String, timeout_s: u64 },
    #[error("артефакт «{path}» не найден в песочнице")]
    ArtifactMissing { path: String },
    #[error("артефакт «{path}» меньше {min_size} байт")]
    ArtifactTooSmall { path: String, min_size: u64 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type SandboxResult<T> = Result<T, SandboxError>;

/// Доказательство исполнения: то, что исполнитель показывает заказчику.
/// Наследие acp-mvp: exec_hash от stdout+stderr, коды возврата, метрики.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProofDocument {
    /// SHA-256 от всех stdout+stderr всех команд.
    pub exec_hash: String,
    /// Код возврата каждой команды.
    pub exit_codes: Vec<i32>,
    /// Найденные артефакты: путь → размер.
    pub artifacts: Vec<ArtifactFound>,
    /// Длительность исполнения, миллисекунды.
    pub duration_ms: u64,
    /// Все команды вернули ожидаемое?
    pub ok: bool,
    /// Хвост вывода для людей.
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactFound {
    pub path: String,
    pub size: u64,
}

/// Интерфейс песочницы. Локальная реализация — в этом модуле; docker/TEE
/// реализуют тот же трейт и подменяются без изменения протокола.
pub trait Sandbox: Send + Sync {
    fn execute(&self, workdir: &Path, verification: &Verification) -> SandboxResult<ProofDocument>;
}

/// Локальная песочница v1.0: процессы с тайм-аутами, без сетевой изоляции.
/// Каждый контракт исполняется в своей рабочей папке.
#[derive(Debug, Clone)]
pub struct LocalSandbox {
    /// Тайм-аут команды по умолчанию, если контракт не задал свой.
    pub default_timeout_s: u64,
}

impl Default for LocalSandbox {
    fn default() -> Self {
        LocalSandbox {
            default_timeout_s: 120,
        }
    }
}

impl Sandbox for LocalSandbox {
    fn execute(&self, workdir: &Path, verification: &Verification) -> SandboxResult<ProofDocument> {
        let started = std::time::Instant::now();
        let mut combined_output: Vec<u8> = Vec::new();
        let mut exit_codes: Vec<i32> = Vec::new();
        let mut excerpt_lines: Vec<String> = Vec::new();

        for test in &verification.tests {
            let timeout_s = if test.timeout_s > 0 {
                test.timeout_s
            } else {
                self.default_timeout_s
            };
            let (code, out, err) = run_command(&test.cmd, workdir, Duration::from_secs(timeout_s))?;
            combined_output.extend_from_slice(&out);
            combined_output.extend_from_slice(&err);
            let text = String::from_utf8_lossy(&out);
            for line in text.lines().take(5) {
                excerpt_lines.push(line.to_string());
            }
            exit_codes.push(code);
        }

        let artifacts = check_artifacts(workdir, &verification.artifacts_expected)?;
        // ok: все коды 0 (контракт ожидает успех) и артефакты найдены
        let ok = exit_codes.iter().all(|&code| code == 0)
            && artifacts.len() == verification.artifacts_expected.len();

        let tail: Vec<String> = {
            let text = String::from_utf8_lossy(&combined_output);
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(3);
            lines[start..].iter().map(|s| s.to_string()).collect()
        };
        let excerpt = if excerpt_lines.is_empty() {
            tail.join("\n")
        } else {
            excerpt_lines.join("\n")
        };

        Ok(ProofDocument {
            exec_hash: pactum_core::hash::exec_hash(&[], &combined_output),
            exit_codes,
            artifacts,
            duration_ms: started.elapsed().as_millis() as u64,
            ok,
            excerpt,
        })
    }
}

/// Сухая проверка контракта без денег: прогнать verification в папке.
pub fn dry_run(workdir: &Path, contract: &Contract) -> SandboxResult<ProofDocument> {
    LocalSandbox::default().execute(workdir, &contract.verification)
}

#[cfg(unix)]
fn run_command(cmd: &str, dir: &Path, timeout: Duration) -> SandboxResult<(i32, Vec<u8>, Vec<u8>)> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd).current_dir(dir);
    run_with_poll(command, cmd, timeout)
}

#[cfg(not(unix))]
fn run_command(cmd: &str, dir: &Path, timeout: Duration) -> SandboxResult<(i32, Vec<u8>, Vec<u8>)> {
    let mut command = Command::new("cmd");
    command.arg("/C").arg(cmd).current_dir(dir);
    run_with_poll(command, cmd, timeout)
}

fn run_with_poll(
    mut command: Command,
    cmd: &str,
    timeout: Duration,
) -> SandboxResult<(i32, Vec<u8>, Vec<u8>)> {
    use std::io::Read;
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|err| SandboxError::Spawn {
            cmd: cmd.to_string(),
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
            break Err(SandboxError::Timeout {
                cmd: cmd.to_string(),
                timeout_s: timeout.as_secs(),
            });
        }
        std::thread::sleep(Duration::from_millis(20));
    }?;
    let mut out = Vec::new();
    let mut err = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut out);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut err);
    }
    Ok((status.code().unwrap_or(-1), out, err))
}

fn check_artifacts(
    workdir: &Path,
    expected: &[ArtifactExpect],
) -> SandboxResult<Vec<ArtifactFound>> {
    let mut found = Vec::new();
    for expect in expected {
        let matches = list_matching(workdir, &expect.path)?;
        let Some(best) = matches.iter().max_by_key(|(_, size)| *size) else {
            return Err(SandboxError::ArtifactMissing {
                path: expect.path.clone(),
            });
        };
        if best.1 < expect.min_size {
            return Err(SandboxError::ArtifactTooSmall {
                path: expect.path.clone(),
                min_size: expect.min_size,
            });
        }
        found.push(ArtifactFound {
            path: best.0.display().to_string(),
            size: best.1,
        });
    }
    Ok(found)
}

/// Простое сопоставление glob (`*`, `?`) по файлам рабочей папки (рекурсивно,
/// глубина ограничена — песочница не ходит по всему диску).
fn list_matching(workdir: &Path, pattern: &str) -> SandboxResult<Vec<(PathBuf, u64)>> {
    let mut out = Vec::new();
    walk(workdir, 0, &mut |path| {
        let rel = path.strip_prefix(workdir).unwrap_or(path).to_path_buf();
        let rel_text = rel.to_string_lossy().replace('\\', "/");
        if pactum_core::schema::glob_match(pattern, &rel_text) {
            if let Ok(meta) = path.metadata() {
                if meta.is_file() {
                    out.push((rel, meta.len()));
                }
            }
        }
    });
    Ok(out)
}

fn walk(dir: &Path, depth: usize, visit: &mut dyn FnMut(&Path)) {
    const MAX_DEPTH: usize = 6;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, depth + 1, visit);
        } else {
            visit(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pactum_contract::{TestCmd, Verification};

    fn workdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pactum-sbx-{}-{}",
            std::process::id(),
            pactum_core::id::random_hex(4)
        ));
        std::fs::create_dir_all(dir.join("target")).unwrap();
        dir
    }

    #[test]
    fn executes_and_hashes_output() {
        let dir = workdir();
        let verification = Verification {
            tests: vec![TestCmd {
                cmd: "echo сборка прошла".into(),
                timeout_s: 10,
            }],
            artifacts_expected: vec![],
        };
        let sandbox = LocalSandbox::default();
        let proof = sandbox.execute(&dir, &verification).unwrap();
        assert!(proof.ok);
        assert_eq!(proof.exit_codes, vec![0]);
        assert!(proof.duration_ms < 60_000);
        assert!(proof.excerpt.contains("сборка прошла"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failing_code_marks_not_ok() {
        let dir = workdir();
        let verification = Verification {
            tests: vec![TestCmd {
                cmd: "sh -c 'echo упс >&2; exit 3'".into(),
                timeout_s: 10,
            }],
            artifacts_expected: vec![],
        };
        let sandbox = LocalSandbox::default();
        let proof = sandbox.execute(&dir, &verification).unwrap();
        assert!(!proof.ok);
        assert_eq!(proof.exit_codes, vec![3]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeout_is_honest_error() {
        let dir = workdir();
        let verification = Verification {
            tests: vec![TestCmd {
                cmd: "sleep 30".into(),
                timeout_s: 1,
            }],
            artifacts_expected: vec![],
        };
        let sandbox = LocalSandbox::default();
        let result = sandbox.execute(&dir, &verification);
        assert!(matches!(
            result,
            Err(SandboxError::Timeout { timeout_s: 1, .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn artifacts_checked_with_glob_and_size() {
        let dir = workdir();
        std::fs::write(dir.join("target/box.wasm"), vec![0u8; 2048]).unwrap();
        std::fs::write(dir.join("target/tiny.wasm"), vec![0u8; 10]).unwrap();
        let verification = Verification {
            tests: vec![TestCmd {
                cmd: "true".into(),
                timeout_s: 5,
            }],
            artifacts_expected: vec![ArtifactExpect {
                path: "target/*.wasm".into(),
                min_size: 1024,
            }],
        };
        let sandbox = LocalSandbox::default();
        let proof = sandbox.execute(&dir, &verification).unwrap();
        assert_eq!(proof.artifacts.len(), 1);
        assert_eq!(proof.artifacts[0].size, 2048);

        // слишком маленький
        let strict = Verification {
            tests: vec![TestCmd {
                cmd: "true".into(),
                timeout_s: 5,
            }],
            artifacts_expected: vec![ArtifactExpect {
                path: "target/*.wasm".into(),
                min_size: 5000,
            }],
        };
        assert!(matches!(
            sandbox.execute(&dir, &strict),
            Err(SandboxError::ArtifactTooSmall { min_size: 5000, .. })
        ));
        // отсутствующий
        let missing = Verification {
            tests: vec![TestCmd {
                cmd: "true".into(),
                timeout_s: 5,
            }],
            artifacts_expected: vec![ArtifactExpect {
                path: "target/*.so".into(),
                min_size: 0,
            }],
        };
        assert!(matches!(
            sandbox.execute(&dir, &missing),
            Err(SandboxError::ArtifactMissing { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exec_hash_changes_with_output() {
        let dir = workdir();
        let verification = Verification {
            tests: vec![TestCmd {
                cmd: "echo v1".into(),
                timeout_s: 5,
            }],
            artifacts_expected: vec![],
        };
        let sandbox = LocalSandbox::default();
        let p1 = sandbox.execute(&dir, &verification).unwrap();
        let verification2 = Verification {
            tests: vec![TestCmd {
                cmd: "echo v2".into(),
                timeout_s: 5,
            }],
            artifacts_expected: vec![],
        };
        let p2 = sandbox.execute(&dir, &verification2).unwrap();
        assert_ne!(p1.exec_hash, p2.exec_hash);
        std::fs::remove_dir_all(&dir).ok();
    }
}
