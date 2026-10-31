//! `pactum scan` — наблюдение за источниками и очередь кандидатов.

use clap::Subcommand;
use pactum_scan::{CandidateQueue, CandidateStatus, HttpFetcher, ScanOptions, Scanner, Watchlist};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum ScanCommand {
    /// Один прогон сканера: обход списка, кандидаты в очередь
    Run {
        #[arg(long, default_value = "watchlist.json")]
        watchlist: PathBuf,
        #[arg(long, default_value = "data/scan.json")]
        state: PathBuf,
        #[arg(long, default_value = "data/candidates.jsonl")]
        queue: PathBuf,
        /// Опубликовать новых кандидатов в хаб (сбор и доставка разделены)
        #[arg(long)]
        publish: bool,
        /// Адрес хаба для --publish
        #[arg(long)]
        hub: Option<String>,
    },
    /// Показать кандидатов очереди
    Candidates {
        #[arg(long, default_value = "data/candidates.jsonl")]
        file: PathBuf,
        /// Фильтр по статусу: new | accepted | rejected
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        show_sample: bool,
    },
    /// Подтвердить кандидата: пусть уходит в хаб
    Accept {
        #[arg(long, default_value = "data/candidates.jsonl")]
        file: PathBuf,
        id: String,
    },
    /// Отклонить кандидата — с причиной
    Reject {
        #[arg(long, default_value = "data/candidates.jsonl")]
        file: PathBuf,
        id: String,
        #[arg(long)]
        reason: String,
    },
}

pub fn run(command: ScanCommand) -> i32 {
    match command {
        ScanCommand::Run {
            watchlist,
            state,
            queue,
            publish,
            hub,
        } => {
            let list = match Watchlist::load(&watchlist) {
                Ok(list) => list,
                Err(err) => {
                    eprintln!("список не прочитан: {err}");
                    return 1;
                }
            };
            let mut scan_state = pactum_scan::ScanState::load(&state);
            let candidates = match CandidateQueue::open(&queue) {
                Ok(queue) => queue,
                Err(err) => {
                    eprintln!("очередь не открылась: {err}");
                    return 1;
                }
            };
            let mut scanner = Scanner::new(&HttpFetcher);
            let options = ScanOptions {
                publish,
                verbose: false,
            };
            let report = scanner.run_once(&list, &mut scan_state, &candidates, &options);
            if scan_state.save(&state).is_err() {
                eprintln!("состояние не сохранилось");
            }
            println!("{}", report.summary());
            for (name, reason) in &report.failed {
                eprintln!("ошибка: {name}: {reason}");
            }
            for name in &report.skipped_volatile {
                eprintln!("изменчивый источник пропущен: {name} (пометите allow_volatile, если изменения настоящие)");
            }
            if publish {
                let Some(hub) = hub else {
                    eprintln!("--publish требует --hub");
                    return 2;
                };
                let pending = candidates
                    .by_status(CandidateStatus::New)
                    .unwrap_or_default();
                let agent = ureq::Agent::new();
                for candidate in pending {
                    let topic = candidate.topic.replace(' ', "-");
                    let body = format!(
                        "источник «{}» изменился (хеш {})\n{}",
                        candidate.source, candidate.new_hash, candidate.sample
                    );
                    let url = format!("{hub}/hub/publish?topic={}", urlencode(&topic));
                    let result = agent
                        .post(&url)
                        .set("Content-Type", "text/plain; charset=utf-8")
                        .send_string(&body)
                        .map(|_| ())
                        .map_err(|err| err.to_string());
                    match result {
                        Ok(()) => println!("опубликовано: {} → {topic}", candidate.source),
                        Err(err) => eprintln!("публикация не прошла: {err}"),
                    }
                }
            }
            if report.failed.is_empty() {
                0
            } else {
                1
            }
        }
        ScanCommand::Candidates {
            file,
            status,
            show_sample,
        } => {
            let queue = match CandidateQueue::open(&file) {
                Ok(queue) => queue,
                Err(err) => {
                    eprintln!("очередь не открылась: {err}");
                    return 1;
                }
            };
            let all = queue.all().unwrap_or_default();
            let filtered: Vec<_> = all
                .into_iter()
                .filter(|candidate| match status.as_deref() {
                    Some("new") => candidate.status == CandidateStatus::New,
                    Some("accepted") => candidate.status == CandidateStatus::Accepted,
                    Some("rejected") => candidate.status == CandidateStatus::Rejected,
                    _ => true,
                })
                .collect();
            if filtered.is_empty() {
                println!("кандидатов нет");
                return 0;
            }
            for candidate in &filtered {
                println!(
                    "{}  {}  {}  хеш={}",
                    candidate.id,
                    status_str(candidate.status),
                    candidate.source,
                    candidate.new_hash
                );
                if show_sample {
                    for line in candidate.sample.lines() {
                        println!("    {line}");
                    }
                }
            }
            0
        }
        ScanCommand::Accept { file, id } => {
            apply_status(&file, &id, CandidateStatus::Accepted, None)
        }
        ScanCommand::Reject { file, id, reason } => {
            apply_status(&file, &id, CandidateStatus::Rejected, Some(&reason))
        }
    }
}

fn apply_status(file: &PathBuf, id: &str, status: CandidateStatus, reason: Option<&str>) -> i32 {
    let queue = match CandidateQueue::open(file) {
        Ok(queue) => queue,
        Err(err) => {
            eprintln!("очередь не открылась: {err}");
            return 1;
        }
    };
    match queue.set_status(id, status, reason) {
        Ok(true) => {
            println!("кандидат {id}: {}", status_str(status));
            0
        }
        Ok(false) => {
            eprintln!("кандидат {id} не найден");
            1
        }
        Err(err) => {
            eprintln!("очередь не записана: {err}");
            1
        }
    }
}

fn status_str(status: CandidateStatus) -> &'static str {
    match status {
        CandidateStatus::New => "новый",
        CandidateStatus::Accepted => "принят",
        CandidateStatus::Rejected => "отклонён",
    }
}

fn urlencode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
