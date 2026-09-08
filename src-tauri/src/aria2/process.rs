use super::{
    rpc::Aria2RpcClient,
    types::{Aria2Error, RpcSecret},
};
use rand::RngCore;
use std::{
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
use tokio::time::{timeout_at, Instant};

#[derive(Clone, Debug)]
pub struct Aria2LaunchConfig {
    pub binary: PathBuf,
    pub data_dir: PathBuf,
    pub download_dir: PathBuf,
    pub parent_pid: u32,
    /// Enable BT peer discovery in production; loopback fixtures must explicitly disable it.
    pub peer_discovery: bool,
}

/// Contains the transient secret: pass directly to Command, never log these arguments.
pub fn build_aria2_args(config: &Aria2LaunchConfig, port: u16, secret: &RpcSecret) -> Vec<String> {
    let session = config.data_dir.join("aria2.session");
    vec![
        "--no-conf=true".into(),
        "--enable-rpc=true".into(),
        "--rpc-listen-all=false".into(),
        "--rpc-allow-origin-all=false".into(),
        "--disable-ipv6=true".into(),
        format!("--rpc-listen-port={port}"),
        format!("--rpc-secret={}", secret.hex()),
        "--continue=true".into(),
        format!("--input-file={}", session.display()),
        format!("--save-session={}", session.display()),
        "--save-session-interval=10".into(),
        "--force-save=false".into(),
        "--pause-metadata=true".into(),
        format!("--dir={}", config.download_dir.display()),
        format!("--stop-with-process={}", config.parent_pid),
        "--split=8".into(),
        "--max-connection-per-server=8".into(),
        "--max-concurrent-downloads=5".into(),
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
    pub async fn start(config: Aria2LaunchConfig) -> Result<(Self, Aria2RpcClient), Aria2Error> {
        std::fs::create_dir_all(&config.data_dir)?;
        std::fs::create_dir_all(&config.download_dir)?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(config.data_dir.join("aria2.session"))?;
        let mut last_error = Aria2Error::ReadinessTimeout;
        for _ in 0..3 {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let mut bytes = [0; 32];
            rand::rngs::OsRng.fill_bytes(&mut bytes);
            let secret = RpcSecret::from_bytes(bytes);
            let client = Aria2RpcClient::with_timeout(
                format!("http://127.0.0.1:{port}/jsonrpc"),
                secret.clone(),
                Duration::from_secs(2),
            )?;
            let mut command = Command::new(&config.binary);
            command
                .args(build_aria2_args(&config, port, &secret))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x08000000); // CREATE_NO_WINDOW
            }
            drop(listener);
            let child = command.spawn()?;
            let mut process = Self {
                child,
                client: client.clone(),
            };
            match process.wait_ready(Duration::from_secs(5)).await {
                Ok(()) => return Ok((process, client)),
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
