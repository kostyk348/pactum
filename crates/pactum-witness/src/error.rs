//! Ошибки нотариуса. Как везде в Pactum — честные и говорящие.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum WitnessError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("подпись не сходится: {0}")]
    Signature(String),
    #[error("хеш записи не сходится: {0}")]
    Hash(String),
    #[error("идентичность не сходится: {0}")]
    Identity(String),
    #[error("цепочка повреждена в записи #{0}: {1}")]
    ChainBroken(u64, String),
    #[error("запись не найдена: {0}")]
    NotFound(String),
    #[error("тело слишком большое: лимит {limit} байт")]
    TooLarge { limit: usize },
    #[error("url не принят: {0}")]
    BadUrl(String),
    #[error("источник недоступен (HTTP {status}): {detail}")]
    Upstream { status: u16, detail: String },
    #[error("сеть: {0}")]
    Network(String),
    #[error("слишком много переходов: >{0}")]
    TooManyRedirects(u32),
}

pub type WitnessResult<T> = Result<T, WitnessError>;
