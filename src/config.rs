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
    pub device: PathBuf,
    pub request_timeout_ms: u64,
    pub max_frame_size: usize,
    pub max_connections: usize,
    pub queue_depth: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device: PathBuf::from("/dev/ipmi0"),
            request_timeout_ms: 3000,
            max_frame_size: 303,
            max_connections: 128,
            queue_depth: 128,
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
        if !(1..=4096).contains(&self.max_connections) {
            return Err(RelayError::Config(
                "max_connections must be 1..=4096".into(),
            ));
        }
        if !(1..=4096).contains(&self.queue_depth) {
            return Err(RelayError::Config("queue_depth must be 1..=4096".into()));
        }
        Ok(())
    }

    pub fn request_timeout(&self) -> Duration {
        Duration::from_millis(self.request_timeout_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_removed_socket_setting() {
        let result = toml::from_str::<Config>(
            r#"
socket = "/run/qemu-ipmi-relay/old.sock"
device = "/dev/ipmi0"
"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn validates_queue_depth() {
        let invalid = Config {
            queue_depth: 0,
            ..Config::default()
        };
        assert!(invalid.validate().is_err());
        let valid = Config {
            queue_depth: 4096,
            ..Config::default()
        };
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn validates_max_connections() {
        let invalid = Config {
            max_connections: 0,
            ..Config::default()
        };
        assert!(invalid.validate().is_err());
        let valid = Config {
            max_connections: 4096,
            ..Config::default()
        };
        assert!(valid.validate().is_ok());
    }
}
