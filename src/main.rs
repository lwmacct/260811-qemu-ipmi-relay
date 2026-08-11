use qemu_ipmi_relay::{
    config::Config,
    error::{RelayError, Result},
    openipmi::OpenIpmi,
    relay::{serve_connection, start_bmc_worker},
};
use std::{
    env,
    os::{fd::FromRawFd, unix::net::UnixListener},
    path::PathBuf,
    process,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const SYSTEMD_LISTEN_FDS_START: i32 = 3;

struct ConnectionPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("qemu-ipmi-relay: {error}");
        process::exit(1);
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
    let listener = systemd_listener()?;
    let backend = wait_for_backend(
        config.device_wait_timeout(),
        config.device_retry_interval(),
        || OpenIpmi::open(&config.device),
    )?;
    info!(device = %config.device.display(), "OpenIPMI device ready");
    let (dispatcher, bmc_worker) =
        start_bmc_worker(backend, config.request_timeout(), config.queue_depth)?;
    thread::Builder::new()
        .name("qemu-ipmi-bmc-supervisor".into())
        .spawn(move || {
            match bmc_worker.join() {
                Ok(()) => error!("BMC worker exited unexpectedly"),
                Err(_) => error!("BMC worker panicked"),
            }
            process::exit(1);
        })?;
    info!(
        device = %config.device.display(),
        max_connections = config.max_connections,
        queue_depth = config.queue_depth,
        "multi-client relay listening"
    );
    let mut next_connection_id = 1u64;
    let active_connections = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let stream = stream?;
        let Some(connection_permit) =
            acquire_connection(Arc::clone(&active_connections), config.max_connections)
        else {
            warn!(
                max_connections = config.max_connections,
                "rejecting QEMU connection at capacity"
            );
            continue;
        };
        let connection_id = next_connection_id;
        next_connection_id = next_connection_id.wrapping_add(1);
        let connection_dispatcher = dispatcher.clone();
        let max_frame_size = config.max_frame_size;
        thread::Builder::new()
            .name(format!("qemu-ipmi-{connection_id}"))
            .spawn(move || {
                let _connection_permit = connection_permit;
                info!(connection_id, "QEMU connected");
                if let Err(error) = serve_connection(stream, &connection_dispatcher, max_frame_size)
                {
                    error!(connection_id, %error, "QEMU connection failed");
                }
                info!(connection_id, "QEMU disconnected");
            })?;
    }
    Ok(())
}

fn acquire_connection(active: Arc<AtomicUsize>, limit: usize) -> Option<ConnectionPermit> {
    active
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
            (count < limit).then_some(count + 1)
        })
        .ok()
        .map(|_| ConnectionPermit { active })
}

fn wait_for_backend<T>(
    timeout: Duration,
    retry_interval: Duration,
    mut open: impl FnMut() -> Result<T>,
) -> Result<T> {
    let deadline = Instant::now() + timeout;
    let mut next_log = Instant::now();
    loop {
        match open() {
            Ok(backend) => return Ok(backend),
            Err(error) => {
                let now = Instant::now();
                if now >= deadline {
                    return Err(RelayError::DeviceWaitTimeout(error.to_string()));
                }
                if now >= next_log {
                    warn!(%error, "OpenIPMI device is not ready; retrying");
                    next_log = now + Duration::from_secs(10);
                }
                thread::sleep(retry_interval.min(deadline.saturating_duration_since(now)));
            }
        }
    }
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

fn systemd_listener() -> Result<UnixListener> {
    let listen_pid =
        env::var("LISTEN_PID").map_err(|_| RelayError::Config("LISTEN_PID is not set".into()))?;
    let listen_fds =
        env::var("LISTEN_FDS").map_err(|_| RelayError::Config("LISTEN_FDS is not set".into()))?;
    let fd = validate_socket_activation(&listen_pid, &listen_fds, process::id())?;
    // SAFETY: systemd passes each activated descriptor starting at fd 3 and
    // transfers ownership to this process. Validation above requires exactly
    // one descriptor for the current PID.
    Ok(unsafe { UnixListener::from_raw_fd(fd) })
}

fn validate_socket_activation(listen_pid: &str, listen_fds: &str, pid: u32) -> Result<i32> {
    let activated_pid = listen_pid
        .parse::<u32>()
        .map_err(|_| RelayError::Config("LISTEN_PID is invalid".into()))?;
    if activated_pid != pid {
        return Err(RelayError::Config(
            "LISTEN_PID does not match this process".into(),
        ));
    }
    let fd_count = listen_fds
        .parse::<u32>()
        .map_err(|_| RelayError::Config("LISTEN_FDS is invalid".into()))?;
    if fd_count != 1 {
        return Err(RelayError::Config(
            "exactly one systemd socket is required".into(),
        ));
    }
    Ok(SYSTEMD_LISTEN_FDS_START)
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

    #[test]
    fn validates_single_systemd_socket() {
        assert_eq!(validate_socket_activation("42", "1", 42).unwrap(), 3);
        assert!(validate_socket_activation("41", "1", 42).is_err());
        assert!(validate_socket_activation("42", "2", 42).is_err());
        assert!(validate_socket_activation("invalid", "1", 42).is_err());
    }

    #[test]
    fn limits_active_connections() {
        let active = Arc::new(AtomicUsize::new(0));
        let first = acquire_connection(Arc::clone(&active), 1).unwrap();
        assert!(acquire_connection(Arc::clone(&active), 1).is_none());
        drop(first);
        assert!(acquire_connection(active, 1).is_some());
    }

    #[test]
    fn retries_backend_until_it_is_ready() {
        let mut attempts = 0;
        let backend = wait_for_backend(Duration::from_secs(1), Duration::from_millis(1), || {
            attempts += 1;
            if attempts < 3 {
                return Err(RelayError::Io(std::io::Error::from(
                    std::io::ErrorKind::NotFound,
                )));
            }
            Ok(42)
        })
        .unwrap();
        assert_eq!(backend, 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn reports_backend_wait_timeout() {
        let error = wait_for_backend(Duration::ZERO, Duration::from_millis(1), || {
            Err::<(), _>(RelayError::Io(std::io::Error::from(
                std::io::ErrorKind::NotFound,
            )))
        })
        .unwrap_err();
        assert!(matches!(error, RelayError::DeviceWaitTimeout(_)));
    }
}
