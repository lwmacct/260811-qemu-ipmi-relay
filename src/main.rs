use qemu_ipmi_relay::{
    config::Config,
    error::{RelayError, Result},
    openipmi::OpenIpmi,
    relay::serve_connection,
};
use std::{
    env, fs,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Path, PathBuf},
};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

fn main() {
    if let Err(error) = run() {
        eprintln!("qemu-ipmi-relay: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config_path = parse_args(env::args().skip(1))?;
    let config = match config_path {
        Some(path) => Config::load(&path)?,
        None => Config::default(),
    };
    config.validate()?;
    if let Some(parent) = config.socket.parent() {
        fs::create_dir_all(parent)?;
    }
    remove_stale_socket(&config.socket)?;
    let listener = UnixListener::bind(&config.socket)?;
    fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o660))?;
    let backend = OpenIpmi::open(&config.device)?;
    info!(socket = %config.socket.display(), device = %config.device.display(), "relay listening");
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = serve_connection(
                    stream,
                    &backend,
                    config.request_timeout(),
                    config.max_frame_size,
                ) {
                    error!(%error, "connection failed");
                }
            }
            Err(error) => return Err(RelayError::Io(error)),
        }
    }
    Ok(())
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Option<PathBuf>> {
    let args = args.collect::<Vec<_>>();
    match args.as_slice() {
        [] => Ok(None),
        [flag, path] if flag == "--config" => Ok(Some(PathBuf::from(path))),
        _ => Err(RelayError::Config(
            "usage: qemu-ipmi-relay [--config PATH]".into(),
        )),
    }
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => Err(RelayError::Config(format!(
            "refusing to replace non-socket path {}",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_config_argument() {
        assert_eq!(
            parse_args(["--config".into(), "/tmp/relay.toml".into()].into_iter()).unwrap(),
            Some(PathBuf::from("/tmp/relay.toml"))
        );
        assert!(parse_args(["--unknown".into()].into_iter()).is_err());
    }
}
