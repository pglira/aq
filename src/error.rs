use thiserror::Error;

/// Errors with a defined exit code. All other errors exit with code 1.
#[derive(Debug, Error)]
pub enum AqError {
    #[error("{0}")]
    Usage(String),
    #[error("{0}")]
    Conflict(String),
    #[error("no aq queue found: run `aq init`")]
    NoQueue,
    #[error("timeout: no change in the queue")]
    Timeout,
}

impl AqError {
    pub fn exit_code(&self) -> i32 {
        match self {
            AqError::Usage(_) => 1,
            AqError::Conflict(_) => 2,
            AqError::NoQueue => 3,
            AqError::Timeout => 4,
        }
    }
}

pub fn usage(msg: impl Into<String>) -> anyhow::Error {
    AqError::Usage(msg.into()).into()
}

pub fn conflict(msg: impl Into<String>) -> anyhow::Error {
    AqError::Conflict(msg.into()).into()
}
