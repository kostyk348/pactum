//! Общие ошибки ядра.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("письмо повреждено: {0}")]
    Letter(String),

    #[error("схема нарушена: {0}")]
    Schema(String),

    #[error("цепочка фактов повреждена на блоке {0}: {1}")]
    ChainBroken(u64, String),

    #[error("{0}")]
    Other(String),
}

pub type CoreResult<T> = Result<T, CoreError>;
