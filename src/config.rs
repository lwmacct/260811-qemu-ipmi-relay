use crate::error::{RelayError, Result};
use serde::Deserialize;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub socket: PathBuf,
    pub device: PathBuf,
    pub request_timeout_ms: u64,
    pub max_frame_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            socket: PathBuf::from("/run/qemu-ipmi-relay/ipmi.sock"),
            device: PathBuf::from("/dev/ipmi0"),
            request_timeout_ms: 3000,
            max_frame_size: 303,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let value = fs::read_to_string(path)?;
        let config: Self = toml::from_str(&value)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !self.socket.is_absolute() {
            return Err(RelayError::Config("socket must be an absolute path".into()));
        }
        if !self.device.is_absolute() {
            return Err(RelayError::Config("device must be an absolute path".into()));
        }
        if self.request_timeout_ms == 0 || self.request_timeout_ms > 60_000 {
            return Err(RelayError::Config(
                "request_timeout_ms must be 1..=60000".into(),
            ));
        }
        if !(8..=4096).contains(&self.max_frame_size) {
            return Err(RelayError::Config("max_frame_size must be 8..=4096".into()));
        }
        Ok(())
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}
