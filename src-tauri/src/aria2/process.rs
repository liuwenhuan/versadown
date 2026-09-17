use super::{
    rpc::{Aria2DebugLogger, Aria2RpcClient},
    types::{Aria2Error, RpcSecret},
};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{timeout_at, Instant};

const STARTUP_RPC_TIMEOUT: Duration = Duration::from_secs(2);
const OPERATION_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STARTUP_STDERR_BYTES: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub struct Aria2LaunchConfig {
    pub binary: PathBuf,
    pub data_dir: PathBuf,
    pub download_dir: PathBuf,
    pub parent_pid: u32,
    /// Enable BT peer discovery in production; loopback fixtures must explicitly disable it.
    pub peer_discovery: bool,
    pub aria2: crate::Aria2Settings,
}

/// Contains the transient secret: pass directly to Command, never log these arguments.
pub fn build_aria2_args(config: &Aria2LaunchConfig, secret: &RpcSecret) -> Vec<String> {
    let session = config.data_dir.join("aria2-next.tasks");
    let state_dir = config.data_dir.join("aria2-next-state");
    let mut args = vec![
        "--no-conf=true".into(),
        "--enable-rpc=true".into(),
        "--rpc-listen-all=false".into(),
        "--rpc-allow-origin-all=false".into(),
        "--disable-ipv6=true".into(),
        format!("--rpc-listen-port={}", config.aria2.rpc_port),
        format!("--rpc-secret={}", secret.hex()),
        "--continue=true".into(),
        format!("--state-dir={}", state_dir.display()),
        format!("--input-file={}", session.display()),
        format!("--save-session={}", session.display()),
        "--save-session-interval=10".into(),
        "--force-save=false".into(),
        "--pause-metadata=true".into(),
        format!("--dir={}", config.download_dir.display()),
        format!("--stop-with-process={}", config.parent_pid),
        format!("--split={}", config.aria2.max_connections_per_server),
        format!(
            "--max-connection-per-server={}",
            config.aria2.max_connections_per_server
        ),
        format!(
            "--max-concurrent-downloads={}",
            config.aria2.max_concurrent_downloads
        ),
        format!("--seed-ratio={}", config.aria2.bt_seed_ratio),
        format!(
            "--max-overall-download-limit={}",
            config.aria2.overall_download_limit
        ),
        format!(
            "--max-overall-upload-limit={}",
            config.aria2.p2p_upload_limit
        ),
        format!("--enable-dht={}", config.peer_discovery),
        "--enable-dht6=false".into(),
        format!("--bt-enable-lpd={}", config.peer_discovery),
        "--summary-interval=0".into(),
        "--console-log-level=error".into(),
    ];
    if config.aria2.debug_logging {
        args.push(format!(
            "--log={}",
            config.data_dir.join("aria2-debug.log").display()
        ));
        args.push("--log-level=debug".into());
    }
    args
}

#[derive(Debug)]
pub struct Aria2Process {
    child: Child,
    client: Aria2RpcClient,
    startup_stderr: Option<std::process::ChildStderr>,
    startup_stderr_log: Option<PathBuf>,
    rpc_secret: RpcSecret,
}

impl Aria2Process {
    pub async fn start(
        mut config: Aria2LaunchConfig,
    ) -> Result<(Self, Aria2RpcClient), Aria2Error> {
        config.aria2.ensure_rpc_token();
        config
            .aria2
            .validate()
            .map_err(|_| Aria2Error::InvalidField {
                field: "aria2Settings",
                value: "invalid".into(),
            })?;
        std::fs::create_dir_all(&config.data_dir)?;
        std::fs::create_dir_all(&config.download_dir)?;
        let legacy_session = config.data_dir.join("aria2.session");
        if legacy_session.is_file() {
            std::fs::remove_file(legacy_session)?;
            let native_tasks = config.data_dir.join("aria2-next.tasks");
            if native_tasks.is_file() {
                std::fs::remove_file(native_tasks)?;
            }
            let native_state = config.data_dir.join("aria2-next-state");
            if native_state.is_dir() {
                std::fs::remove_dir_all(native_state)?;
            }
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.data_dir.join("aria2-next.tasks"))?;
        ensure_rpc_port_available(config.aria2.rpc_port)?;
        let debug_logger = config
            .aria2
            .debug_logging
            .then(|| Aria2DebugLogger::open(config.data_dir.join("aria2-rpc-debug.log")))
            .transpose()?;
        let mut last_error = Aria2Error::ReadinessTimeout;
        for _ in 0..3 {
            let secret = RpcSecret::from_token(&config.aria2.rpc_token)?;
            let client = Aria2RpcClient::with_timeout(
                format!("http://127.0.0.1:{}/jsonrpc", config.aria2.rpc_port),
                secret.clone(),
                STARTUP_RPC_TIMEOUT,
            )?
            .with_debug_logger(debug_logger.clone());
            let mut command = Command::new(&config.binary);
            command
                .args(build_aria2_args(&config, &secret))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                // Keep early sidecar failures in a private pipe until the
                // process exits. They are redacted before being persisted.
                .stderr(if debug_logger.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                });
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            let mut child = command.spawn()?;
            let mut process = Self {
                startup_stderr: child.stderr.take(),
                child,
                client: client.clone(),
                startup_stderr_log: debug_logger
                    .as_ref()
                    .map(|_| config.data_dir.join("aria2-startup-debug.log")),
                rpc_secret: secret.clone(),
            };
            match process.wait_ready(Duration::from_secs(5)).await {
                Ok(()) => {
                    // Startup probes should fail fast, but an established
                    // aria2 instance can take longer to answer while opening
                    // several authenticated HTTP connections. Returning the
                    // two-second readiness client made a valid cloud task
                    // appear to fail before aria2 received its first bytes.
                    let client = Aria2RpcClient::with_timeout(
                        format!("http://127.0.0.1:{}/jsonrpc", config.aria2.rpc_port),
                        secret,
                        OPERATION_RPC_TIMEOUT,
                    )?
                    .with_debug_logger(debug_logger.clone());
                    process.client = client.clone();
                    return Ok((process, client));
                }
                Err(error) => {
                    last_error = error;
                    process.terminate_fallback()?;
                }
            }
        }
        Err(last_error)
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }
    pub fn is_running(&mut self) -> Result<bool, Aria2Error> {
        Ok(self.child.try_wait()?.is_none())
    }
    pub async fn wait_ready(&mut self, duration: Duration) -> Result<(), Aria2Error> {
        let deadline = Instant::now() + duration;
        loop {
            if !self.is_running()? {
                return Err(Aria2Error::ProcessExited);
            }
            match timeout_at(deadline, self.client.get_version()).await {
                Ok(Ok(_)) => return Ok(()),
                Err(_) => {
                    return if self.is_running()? {
                        Err(Aria2Error::ReadinessTimeout)
                    } else {
                        Err(Aria2Error::ProcessExited)
                    }
                }
                _ => {}
            }
            if !self.is_running()? {
                return Err(Aria2Error::ProcessExited);
            }
            if Instant::now() >= deadline {
                return Err(Aria2Error::ReadinessTimeout);
            }
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(50)).min(deadline))
                .await;
        }
    }

    pub async fn wait_for_exit(&mut self, duration: Duration) -> Result<bool, Aria2Error> {
        let deadline = Instant::now() + duration;
        loop {
            if !self.is_running()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(50)).min(deadline))
                .await;
        }
    }

    pub fn terminate_fallback(&mut self) -> Result<(), Aria2Error> {
        if self.is_running()? {
            self.child.kill()?;
        }
        self.child.wait()?;
        self.persist_startup_stderr();
        Ok(())
    }

    fn persist_startup_stderr(&mut self) {
        let (Some(stderr), Some(path)) =
            (self.startup_stderr.take(), self.startup_stderr_log.take())
        else {
            return;
        };
        let mut bytes = Vec::new();
        if stderr
            .take(MAX_STARTUP_STDERR_BYTES)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.is_empty()
        {
            return;
        }
        let output = self.rpc_secret.redact(&String::from_utf8_lossy(&bytes));
        if let Ok(mut log) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(log, "{output}");
        }
    }
}

fn ensure_rpc_port_available(port: u16) -> Result<(), Aria2Error> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            Err(Aria2Error::RpcPortInUse(port))
        }
        Err(error) => Err(Aria2Error::Io(error)),
    }
}
impl Drop for Aria2Process {
    fn drop(&mut self) {
        let _ = self.terminate_fallback();
    }
}

pub async fn wait_rpc_ready(client: &Aria2RpcClient, duration: Duration) -> Result<(), Aria2Error> {
    let deadline = Instant::now() + duration;
    loop {
        match timeout_at(deadline, client.get_version()).await {
            Ok(Ok(_)) => return Ok(()),
            Err(_) => return Err(Aria2Error::ReadinessTimeout),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(Aria2Error::ReadinessTimeout);
        }
        tokio::time::sleep_until((Instant::now() + Duration::from_millis(50)).min(deadline)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_persistent_download_and_p2p_upload_limits_to_aria2_next() {
        let config = Aria2LaunchConfig {
            binary: PathBuf::from("aria2c"),
            data_dir: PathBuf::from("data"),
            download_dir: PathBuf::from("downloads"),
            parent_pid: 42,
            peer_discovery: false,
            aria2: crate::Aria2Settings {
                overall_download_limit: 2_621_440,
                p2p_upload_limit: 1_572_864,
                ..Default::default()
            },
        };

        let args = build_aria2_args(&config, &RpcSecret::from_token("test-token").unwrap());

        assert!(args.contains(&"--max-overall-download-limit=2621440".to_string()));
        assert!(args.contains(&"--max-overall-upload-limit=1572864".to_string()));
    }
}
