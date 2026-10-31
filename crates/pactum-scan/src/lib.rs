//! # pactum-scan
//!
//! Сканер источников — сбор изменений с честным разделением труда.
//!
//! Порт `hubd`-сканера в состав Pactum. Сбор и доставка разделены: сканер
//! кладёт найденные изменения в очередь кандидатов, человек (или владелец
//! узла) решает, что публиковать. Сканер не разбирает содержимое и не
//! принимает решений — только фиксирует: «здесь что-то изменилось».
//!
//! Универсальность: источник — не обязательно веб-страница.
//!
//! * `url` — документ в сети (условный GET: `If-None-Match`/`If-Modified-Since`);
//! * `file` — файл на диске (отчёты, логи, конфиги);
//! * `cmd` — вывод команды (остаток места, очередь задач, статус CI).
//!
//! Дедупликация — SHA-256 содержимого. Вежливость — интервал на источник
//! и лимит на хост. Изменчивые источники (команды с таймстампами в выводе)
//! обязаны явно помечаться `allow_volatile`, иначе их «изменения» честно
//! пропускаются с пояснением.

pub mod candidates;
pub mod fetch;
pub mod scanner;
pub mod source;

pub use candidates::{Candidate, CandidateQueue, CandidateStatus};
pub use fetch::{FetchOutcome, Fetcher, HttpFetcher, MockFetcher};
pub use scanner::{ScanOptions, ScanReport, ScanState, Scanner};
pub use source::{Source, SourceSpec, Watchlist};
