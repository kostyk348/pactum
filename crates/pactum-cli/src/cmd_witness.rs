//! `pactum witness` — нотариус контента: подписанные приёмники, журнал
//! с хеш-цепочкой, checkpoint-ы в хаб. Без денег: только факты и подписи.

use clap::Subcommand;
use pactum_identity::{load_keystore, save_keystore, KeyPair};
use pactum_witness::journal::{Journal, JOURNAL_FILE};
use pactum_witness::{WitnessConfig, WitnessService};
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum WitnessCommand {
    /// Поднять нотариуса: REST API + журнал + blob-хранилище
    Serve {
        /// Рабочая папка (журнал, blobs, ключ)
        #[arg(long, default_value = "witness-data")]
        dir: PathBuf,
        #[arg(long, default_value = "127.0.0.1:8090")]
        listen: String,
        /// Потолок тела notarize, МиБ
        #[arg(long, default_value_t = 16)]
        max_mb: u64,
        /// Запросов на IP в минуту
        #[arg(long, default_value_t = 240)]
        rate_per_min: u32,
        /// Файл ключа (создастся, если нет)
        #[arg(long)]
        key: Option<PathBuf>,
        /// Фраза keystore (иначе WITNESS_PASSPHRASE; пусто — ключ открытым текстом)
        #[arg(long)]
        passphrase: Option<String>,
        /// Файл с фразой keystore (первая строка; для systemd)
        #[arg(long)]
        passphrase_file: Option<PathBuf>,
        /// Хаб для checkpoint-ов: base url (например http://127.0.0.1:8080)
        #[arg(long)]
        hub_url: Option<String>,
        /// Топик checkpoint-ов в хабе
        #[arg(long, default_value = "witness.head")]
        hub_topic: String,
        /// Каждая N-я запись публикует подписанную голову в хаб
        #[arg(long, default_value_t = 25)]
        checkpoint_every: u64,
        /// Разрешить приватные адреса в /notarize/url (self-hosted сети)
        #[arg(long, default_value_t = false)]
        allow_private_urls: bool,
    },
    /// Нотаризовать файл локально, без сервера: приёмник в stdout
    Notarize {
        /// Файл с содержимым
        path: PathBuf,
        /// Рабочая папка журнала
        #[arg(long, default_value = "witness-data")]
        dir: PathBuf,
        /// Заметка в приёмник (по умолчанию — имя файла)
        #[arg(long)]
        note: Option<String>,
        /// Сохранить копию содержимого в blobs/
        #[arg(long, default_value_t = false)]
        store: bool,
        /// Фраза keystore (иначе WITNESS_PASSPHRASE)
        #[arg(long)]
        passphrase: Option<String>,
    },
    /// Проверить приёмник офлайн: подпись, идентичность, принадлежность журналу
    Verify {
        /// Файл приёмника (голый JSON или {"receipt": …} из API)
        receipt_path: PathBuf,
        /// Рабочая папка журнала — проверить, что запись в цепочке
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Пин открытого ключа нотариуса (130 hex): подписал ли он вообще
        #[arg(long)]
        pubkey: Option<String>,
    },
    /// Подписанная голова журнала (checkpoint) + проверка всей цепочки
    Checkpoint {
        #[arg(long, default_value = "witness-data")]
        dir: PathBuf,
        /// Фраза keystore (иначе WITNESS_PASSPHRASE)
        #[arg(long)]
        passphrase: Option<String>,
    },
}

pub fn run(command: WitnessCommand) -> i32 {
    match command {
        WitnessCommand::Serve {
            dir,
            listen,
            max_mb,
            rate_per_min,
            key,
            passphrase,
            passphrase_file,
            hub_url,
            hub_topic,
            checkpoint_every,
            allow_private_urls,
        } => {
            let passphrase = match resolve_passphrase(passphrase, passphrase_file) {
                Ok(pass) => pass,
                Err(err) => {
                    eprintln!("{err}");
                    return 1;
                }
            };
            serve(
                dir,
                &listen,
                max_mb,
                rate_per_min,
                key,
                passphrase,
                hub_url,
                hub_topic,
                checkpoint_every,
                allow_private_urls,
            )
        }
        WitnessCommand::Notarize {
            path,
            dir,
            note,
            store,
            passphrase,
        } => notarize_file(&path, &dir, note, store, passphrase),
        WitnessCommand::Verify {
            receipt_path,
            dir,
            pubkey,
        } => verify_receipt_file(&receipt_path, dir.as_deref(), pubkey.as_deref()),
        WitnessCommand::Checkpoint { dir, passphrase } => checkpoint(&dir, passphrase),
    }
}

#[allow(clippy::too_many_arguments)]
fn serve(
    dir: PathBuf,
    listen: &str,
    max_mb: u64,
    rate_per_min: u32,
    key_path: Option<PathBuf>,
    passphrase: Option<String>,
    hub_url: Option<String>,
    hub_topic: String,
    checkpoint_every: u64,
    allow_private_urls: bool,
) -> i32 {
    let key_path = key_path.unwrap_or_else(|| dir.join("witness.key.json"));
    let key = match load_or_create_key(&key_path, passphrase_from(&passphrase)) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("ключ не готов: {err}");
            return 1;
        }
    };
    let max_body = (max_mb.min(4096) as usize) * 1024 * 1024;
    let cfg = WitnessConfig {
        max_body,
        rate_per_min,
        hub_url,
        hub_topic,
        checkpoint_every,
        url_limits: pactum_witness::fetch::UrlLimits {
            max_bytes: max_body,
            allow_private: allow_private_urls,
            ..pactum_witness::fetch::UrlLimits::default()
        },
    };
    let service = match WitnessService::open(&dir, key, cfg) {
        Ok(service) => service,
        Err(err) => {
            eprintln!("нотариус не поднялся: {err}");
            return 1;
        }
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("рантайм: {err}");
            return 1;
        }
    };
    let service = std::sync::Arc::new(service);
    let handle = service_handle(&service);
    println!("Pactum Witness — нотариус контента (acp/1)");
    println!("  witness: {}", service.node_id());
    println!("  pubkey:  {}", service.pubkey_hex());
    println!("  папка:   {}", dir.display());
    println!("  журнал:  {} записей", handle["receipts"]);
    println!(
        "  хаб:     {}",
        service.config().hub_url.as_deref().unwrap_or("нет")
    );
    println!("  маршруты: POST /notarize · POST /notarize/url · GET /receipts/{{sha256}}");
    println!(
        "            GET /verify/{{sha256}} · GET /chain/head · GET /chain/export · GET /health"
    );
    println!("  слушаю   {listen}");

    let result = rt.block_on(async move {
        let (_addr, task) = match pactum_witness::api::serve_witness(service, listen).await {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("порт не открыт: {err}");
                return 1;
            }
        };
        drop(task); // отвязываем: сервер живёт до Ctrl-C
        let _ = tokio::signal::ctrl_c().await;
        println!("\nостановлено честно: журнал на диске цел");
        0
    });
    result
}

fn notarize_file(
    path: &std::path::Path,
    dir: &std::path::Path,
    note: Option<String>,
    store: bool,
    passphrase: Option<String>,
) -> i32 {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("файл не прочитан: {err}");
            return 1;
        }
    };
    let key_path = dir.join("witness.key.json");
    let key = match load_or_create_key(&key_path, passphrase_from(&passphrase)) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("ключ не готов: {err}");
            return 1;
        }
    };
    let service = match WitnessService::open(dir, key, WitnessConfig::default()) {
        Ok(service) => service,
        Err(err) => {
            eprintln!("журнал не открыт: {err}");
            return 1;
        }
    };
    let note = note.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    match service.notarize(&bytes, None, Some(note), store) {
        Ok(outcome) => {
            if outcome.duplicate {
                eprintln!("такой дайджест уже нотариован — приёмник прежний");
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome.receipt).unwrap_or_default()
            );
            0
        }
        Err(err) => {
            eprintln!("не нотариовано: {err}");
            1
        }
    }
}

fn verify_receipt_file(
    receipt_path: &std::path::Path,
    dir: Option<&std::path::Path>,
    pinned: Option<&str>,
) -> i32 {
    let text = match std::fs::read_to_string(receipt_path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("приёмник не прочитан: {err}");
            return 1;
        }
    };
    // принимаем и голый приёмник, и обёртку {"receipt": …} из API
    let wrapper: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("приёмник не разобран: {err}");
            return 1;
        }
    };
    let receipt: pactum_witness::Receipt = if wrapper.get("sig").is_some() {
        match serde_json::from_value(wrapper.clone()) {
            Ok(receipt) => receipt,
            Err(err) => {
                eprintln!("приёмник не разобран: {err}");
                return 1;
            }
        }
    } else {
        match serde_json::from_value(wrapper.get("receipt").cloned().unwrap_or_default()) {
            Ok(receipt) => receipt,
            Err(err) => {
                eprintln!("в файле нет приёмника: {err}");
                return 1;
            }
        }
    };

    let verify_result = match pinned {
        Some(pubkey) => pactum_witness::verify_receipt_pinned(&receipt, pubkey),
        None => pactum_witness::verify_receipt(&receipt),
    };
    if let Err(err) = verify_result {
        eprintln!("приёмник НЕ подтверждён: {err}");
        return 1;
    }
    println!(
        "подпись верна: witness {} · seq {} · {} · {} байт",
        receipt.witness, receipt.seq, receipt.ts, receipt.size
    );

    if let Some(dir) = dir {
        let journal_path = dir.join(JOURNAL_FILE);
        let journal = match Journal::open(&journal_path) {
            Ok(journal) => journal,
            Err(err) => {
                eprintln!("журнал повреждён или отсутствует: {err}");
                return 1;
            }
        };
        match journal.get(&receipt.content_sha256) {
            Some(recorded) if recorded.entry_hash == receipt.entry_hash => {
                println!(
                    "запись #{} найдена в журнале ({} записей), цепочка цела",
                    recorded.seq,
                    journal.len()
                );
                0
            }
            Some(recorded) => {
                eprintln!(
                    "дайджест в журнале есть, но запись другая (#{}, entry_hash {})",
                    recorded.seq, recorded.entry_hash
                );
                1
            }
            None => {
                eprintln!("в журнале ({}) такого дайджеста нет", journal.len());
                1
            }
        }
    } else {
        println!("журнал не проверялся (--dir не задан) — подпись офлайн-верна");
        0
    }
}

fn checkpoint(dir: &std::path::Path, passphrase: Option<String>) -> i32 {
    let key_path = dir.join("witness.key.json");
    let key = match load_or_create_key(&key_path, passphrase_from(&passphrase)) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("ключ не готов: {err}");
            return 1;
        }
    };
    let service = match WitnessService::open(dir, key, WitnessConfig::default()) {
        Ok(service) => service,
        Err(err) => {
            eprintln!("журнал не открыт: {err}");
            return 1;
        }
    };
    let head = match service.signed_head() {
        Ok(Some(head)) => head,
        Ok(None) => {
            println!("журнал пуст — головы нет");
            return 0;
        }
        Err(err) => {
            eprintln!("голова не подписана: {err}");
            return 1;
        }
    };
    if let Err(err) = head.verify(&service.pubkey_hex()) {
        eprintln!("голова НЕ подтверждена: {err}");
        return 1;
    }
    println!(
        "checkpoint: seq {} · {} · {}",
        head.seq, head.ts, head.head_hash
    );
    println!(
        "{}",
        serde_json::to_string_pretty(&head).unwrap_or_default()
    );
    0
}

fn resolve_passphrase(
    flag: Option<String>,
    file: Option<PathBuf>,
) -> Result<Option<String>, String> {
    if let Some(pass) = flag.filter(|p| !p.is_empty()) {
        return Ok(Some(pass));
    }
    if let Some(path) = file {
        let text = std::fs::read_to_string(&path)
            .map_err(|err| format!("фраза не прочитана из {path:?}: {err}"))?;
        let pass = text.lines().next().unwrap_or_default().trim().to_string();
        if pass.is_empty() {
            return Err(format!("файл фразы {path:?} пуст"));
        }
        return Ok(Some(pass));
    }
    Ok(passphrase_from(&None))
}

fn passphrase_from(flag: &Option<String>) -> Option<String> {
    flag.clone().filter(|p| !p.is_empty()).or_else(|| {
        std::env::var("WITNESS_PASSPHRASE")
            .ok()
            .filter(|p| !p.is_empty())
    })
}

fn load_or_create_key(
    path: &std::path::Path,
    passphrase: Option<String>,
) -> Result<KeyPair, String> {
    if path.exists() {
        load_keystore(path, passphrase.as_deref()).map_err(|err| err.to_string())
    } else {
        let key = KeyPair::generate();
        if passphrase.is_none() {
            eprintln!(
                "внимание: ключ сохранён ОТКРЫТЫМ текстом — задайте --passphrase или WITNESS_PASSPHRASE"
            );
        }
        save_keystore(path, &key, passphrase.as_deref()).map_err(|err| err.to_string())?;
        eprintln!("новый ключ нотариуса: {}", key.node_id());
        Ok(key)
    }
}

fn service_handle(service: &WitnessService) -> serde_json::Value {
    service.summary().unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passphrase_from_flag_beats_env() {
        std::env::set_var("WITNESS_PASSPHRASE", "из-окружения");
        assert_eq!(
            passphrase_from(&Some("из-флага".into())),
            Some("из-флага".into())
        );
        assert_eq!(passphrase_from(&None), Some("из-окружения".into()));
        std::env::remove_var("WITNESS_PASSPHRASE");
    }

    #[test]
    fn key_roundtrip_through_helper() {
        let dir = std::env::temp_dir().join(format!("pactum-wcli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.json");
        let first = load_or_create_key(&path, None).unwrap();
        let second = load_or_create_key(&path, None).unwrap();
        assert_eq!(first.node_id(), second.node_id());
        std::fs::remove_dir_all(&dir).ok();
    }
}
