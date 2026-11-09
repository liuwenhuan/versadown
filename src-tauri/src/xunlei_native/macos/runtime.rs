use super::super::engine::NativeEngineError;
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

// DownloadService shipped with Thunder 5.80.7 (66660), a universal binary.
// Pin its launch metadata too: the plist selects the executable that XPC loads.
const SERVICE_SHA256: &str = "661f0f435dfbfb23ae5788c8f3fe9454f24f43c5e6f4ec5605eb9826b491071d";
const SERVICE_PATH: &str = "Contents/XPCServices/DownloadService.xpc";
const SERVICE_FILES: &[(&str, &str)] = &[
    ("Contents/MacOS/DownloadService", SERVICE_SHA256),
    (
        "Contents/Info.plist",
        "d6d091a26236bf07f1d6a28ace8ee68c3a9025d3cd43857cd3c7bdbbb6b66904",
    ),
    (
        "Contents/_CodeSignature/CodeResources",
        "6686de10a28a2fe11b36cbb86dcbacc827cfc4ea116b4dabf1845e5aee629e9b",
    ),
];
const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/versadown-xunlei-worker"));
const HOST_PLIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.versadown.thunder-worker</string><key>CFBundleExecutable</key><string>worker</string><key>CFBundlePackageType</key><string>APPL</string><key>LSUIElement</key><true/></dict></plist>"#;

pub(super) struct Runtime {
    pub(super) executable: PathBuf,
    pub(super) state: PathBuf,
    _bundle: TempDir,
}

fn private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn install_file(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    private_directory(path.parent().unwrap())?;
    fs::write(path, contents)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

pub(super) fn installed_runtime(root: &Path) -> Result<Runtime, NativeEngineError> {
    let mut candidates = vec![PathBuf::from("/Applications/Thunder.app")];
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(PathBuf::from(home).join("Applications/Thunder.app"));
    }
    let source = candidates
        .into_iter()
        .map(|app| app.join(SERVICE_PATH))
        .find(|service| service.is_dir())
        .ok_or(NativeEngineError::Unavailable)?;
    // NSXPCConnection looks for the service inside its host bundle. Assemble
    // that bundle with only the verified bytes; never execute a cached worker.
    let io_error = |_| NativeEngineError::OperationFailed;
    let temporary = tempfile::Builder::new()
        .prefix("vd-thunder-")
        .tempdir()
        .map_err(io_error)?;
    let bundle = temporary.path().join("VersaDownThunder.app");
    for &(file, expected) in SERVICE_FILES {
        let bytes = fs::read(source.join(file)).map_err(|_| NativeEngineError::Incompatible)?;
        if format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(NativeEngineError::Incompatible);
        }
        let mode = if file == "Contents/MacOS/DownloadService" {
            0o700
        } else {
            0o600
        };
        install_file(&bundle.join(SERVICE_PATH).join(file), &bytes, mode).map_err(io_error)?;
    }
    let executable = bundle.join("Contents/MacOS/worker");
    install_file(&executable, HELPER, 0o700).map_err(io_error)?;
    install_file(&bundle.join("Contents/Info.plist"), HOST_PLIST, 0o600).map_err(io_error)?;
    private_directory(root).map_err(io_error)?;
    let state = root.join(format!("state-{}", &SERVICE_SHA256[..12]));
    private_directory(&state).map_err(io_error)?;
    Ok(Runtime {
        executable,
        state,
        _bundle: temporary,
    })
}
