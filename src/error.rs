use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("configuration parse error: {0}")]
    ConfigParse(#[from] toml::de::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("OpenIPMI error: {0}")]
    OpenIpmi(String),
    #[error("BMC worker is unavailable")]
    BmcWorkerUnavailable,
}

pub type Result<T> = std::result::Result<T, RelayError>;
