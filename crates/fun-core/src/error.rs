use thiserror::Error;

#[derive(Debug, Error)]
pub enum FunError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid project configuration: {0}")]
    Config(String),
    #[error("function `{0}` was not found")]
    FunctionNotFound(String),
    #[error("worker error: {0}")]
    Worker(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FunctionErrorKind {
    User,
    InvalidRequest,
    Internal,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FunctionError {
    pub kind: FunctionErrorKind,
    pub message: String,
}

impl FunctionError {
    pub fn new(kind: FunctionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
