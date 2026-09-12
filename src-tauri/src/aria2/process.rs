use super::{
    rpc::Aria2RpcClient,
    types::{Aria2Error, RpcSecret},
};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{timeout_at, Instant};

const STARTUP_RPC_TIMEOUT: Duration = Duration::from_secs(2);
const OPERATION_RPC_TIMEOUT: Duration = Duration::from_secs(10);

// aria2.session is the restart manifest. aria2-next's stream database only owns
// in-flight transfer state; leaving the same GID in both sources makes startup
// reject the manifest with an identity ownership conflict. Release only those
// overlapping stream identities so the manifest can recreate and resume them.
fn repair_stream_session_identity_conflicts(data_dir: &Path) -> io::Result<usize> {
    let session_path = data_dir.join("aria2.session");
    let state_path = data_dir.join("aria2-next-state/stream/state.db");
    if !session_path.is_file() || !state_path.is_file() {
        return Ok(0);
    }

    let session = std::fs::read_to_string(&session_path)?;
    let session_gids = session
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("gid="))
        .map(str::trim)
        .filter(|gid| !gid.is_empty())
        .map(str::to_owned)
        .collect::<HashSet<_>>();
    if session_gids.is_empty() {
        return Ok(0);
    }

    let mut connection = rusqlite::Connection::open_with_flags(
        state_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    let transaction = connection.transaction().map_err(io::Error::other)?;
    let removed = {
        let mut statement = transaction
            .prepare("DELETE FROM downloads WHERE gid = ?1")
            .map_err(io::Error::other)?;
        let mut removed = 0;
        for gid in session_gids {
            removed += statement
                .execute(rusqlite::params![gid])
                .map_err(io::Error::other)?;
        }
        removed
    };
    transaction.commit().map_err(io::Error::other)?;
    Ok(removed)
}

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
    let session = config.data_dir.join("aria2.session");
    let state_dir = config.data_dir.join("aria2-next-state");
    vec![
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
        format!("--enable-dht={}", config.peer_discovery),
        "--enable-dht6=false".into(),
        format!("--bt-enable-lpd={}", config.peer_discovery),
        "--summary-interval=0".into(),
        "--console-log-level=error".into(),
    ]
}

#[derive(Debug)]
pub struct Aria2Process {
    child: Child,
    client: Aria2RpcClient,
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
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.data_dir.join("aria2.session"))?;
        repair_stream_session_identity_conflicts(&config.data_dir)?;
        let mut last_error = Aria2Error::ReadinessTimeout;
        for _ in 0..3 {
            let secret = RpcSecret::from_token(&config.aria2.rpc_token)?;
            let client = Aria2RpcClient::with_timeout(
                format!("http://127.0.0.1:{}/jsonrpc", config.aria2.rpc_port),
                secret.clone(),
                STARTUP_RPC_TIMEOUT,
            )?;
            let mut command = Command::new(&config.binary);
            command
                .args(build_aria2_args(&config, &secret))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            let child = command.spawn()?;
            let mut process = Self {
                child,
                client: client.clone(),
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
                    )?;
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
                Err(_) => return Err(Aria2Error::ReadinessTimeout),
                _ => {}
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
        Ok(())
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
