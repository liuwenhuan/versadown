use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

const XUNLEI_PACKAGE_ID: &str = "com.xunlei.download";
const DEB_RUNTIME_ROOT: &str = "/opt/apps/com.xunlei.download/files";
const LINGLONG_LAYERS_ROOT: &str = "/var/lib/linglong/layers";
const SDK_LIBRARY_NAME: &str = "libxl_thunder_sdk.so";
const STAT_LIBRARY_NAME: &str = "libxl_stat.so";
const NODE_LIBRARY_NAME: &str = "libnode.so";
const AXDL_ADDON_RELATIVE_PATH: &str = "resources/bin/ThunderKernel.node";

const SUPPORTED_AXDL_FINGERPRINTS: &[(&str, &str, &str, &str)] = &[
    (
        "61bd630d03333149c34c6d0f83fc88d316c508d4e6ae9e7389dd556e0a27eb59",
        "6f8fa291b82a287bb690ab326f4f2aa6b1121ebf3891feb5451745458e7e87ee",
        "f6edaccd68eab37a171f53d3cfaafa26f3c38e1494fd849ad8c88c8c622cf277",
        "d6f8a46883185393c744d12e4724240daaf21e35f3639e9523f0a084440683c2",
    ),
    (
        "61bd630d03333149c34c6d0f83fc88d316c508d4e6ae9e7389dd556e0a27eb59",
        "6f8fa291b82a287bb690ab326f4f2aa6b1121ebf3891feb5451745458e7e87ee",
        "f6edaccd68eab37a171f53d3cfaafa26f3c38e1494fd849ad8c88c8c622cf277",
        "153fe8e22f5e1254c10cf4baf6ee4cc47e28ce9ff46acffe1043db28044bf639",
    ),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LinuxRuntimeSource {
    Deb,
    Linyaps,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LinuxRuntimeIdentity {
    pub(super) source: LinuxRuntimeSource,
    pub(super) version: String,
}

/// This descriptor is reconstructed before every worker launch. A Linyaps
/// layer directory is intentionally runtime-only state: no layer hash is ever
/// written to application settings or task data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LinuxXunleiRuntime {
    pub(super) source: LinuxRuntimeSource,
    pub(super) version: String,
    pub(super) root: PathBuf,
    pub(super) node_library: PathBuf,
    pub(super) stat_library: PathBuf,
    pub(super) sdk_library: PathBuf,
    pub(super) axdl_addon: PathBuf,
    pub(super) node_sha256: String,
    pub(super) stat_sha256: String,
    pub(super) sdk_sha256: String,
    pub(super) axdl_addon_sha256: String,
}

impl LinuxXunleiRuntime {
    pub(super) fn identity(&self) -> LinuxRuntimeIdentity {
        LinuxRuntimeIdentity {
            source: self.source,
            version: self.version.clone(),
        }
    }

    pub(super) fn supports_axdl_bridge(&self) -> bool {
        is_supported_axdl_fingerprint(
            &self.node_sha256,
            &self.stat_sha256,
            &self.sdk_sha256,
            &self.axdl_addon_sha256,
        )
    }
}

pub(super) fn is_supported_axdl_fingerprint(
    node: &str,
    stat: &str,
    sdk: &str,
    addon: &str,
) -> bool {
    SUPPORTED_AXDL_FINGERPRINTS.iter().any(
        |(expected_node, expected_stat, expected_sdk, expected_addon)| {
            node == *expected_node
                && stat == *expected_stat
                && sdk == *expected_sdk
                && addon == *expected_addon
        },
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VersionKey(Vec<u64>);

impl VersionKey {
    fn parse(value: &str) -> Option<Self> {
        let mut parts = value
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        while parts.last() == Some(&0) {
            parts.pop();
        }
        Some(Self(parts))
    }
}

impl Ord for VersionKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl PartialOrd for VersionKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LinyapsPackage {
    id: String,
    version: String,
}

impl LinyapsPackage {
    #[cfg(test)]
    fn new(id: &str, version: &str) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
        }
    }
}

trait RuntimeEnvironment {
    fn installed_deb_version(&self) -> Option<String>;
    fn installed_linyaps_packages(&self) -> Vec<LinyapsPackage>;
    fn deb_root(&self) -> PathBuf;
    fn linyaps_layers_root(&self) -> PathBuf;
}

struct SystemRuntimeEnvironment;

impl RuntimeEnvironment for SystemRuntimeEnvironment {
    fn installed_deb_version(&self) -> Option<String> {
        let output = Command::new("dpkg-query")
            .args([
                "-W",
                "-f=${db:Status-Status}\\t${Version}",
                XUNLEI_PACKAGE_ID,
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let (status, version) = value.trim().split_once('\t')?;
        (status == "installed").then(|| version.to_owned())
    }

    fn installed_linyaps_packages(&self) -> Vec<LinyapsPackage> {
        let output = match Command::new("ll-cli").args(["--json", "list"]).output() {
            Ok(output) if output.status.success() => output,
            _ => return Vec::new(),
        };
        let items: Vec<serde_json::Value> = match serde_json::from_slice(&output.stdout) {
            Ok(items) => items,
            Err(_) => return Vec::new(),
        };
        items
            .into_iter()
            .filter_map(|item| {
                let id = item.get("id")?.as_str()?;
                let version = item.get("version")?.as_str()?;
                (id == XUNLEI_PACKAGE_ID).then(|| LinyapsPackage {
                    id: id.to_owned(),
                    version: version.to_owned(),
                })
            })
            .collect()
    }

    fn deb_root(&self) -> PathBuf {
        PathBuf::from(DEB_RUNTIME_ROOT)
    }

    fn linyaps_layers_root(&self) -> PathBuf {
        PathBuf::from(LINGLONG_LAYERS_ROOT)
    }
}

pub(super) fn discover_best_runtime() -> Option<LinuxXunleiRuntime> {
    discover_with(&SystemRuntimeEnvironment)
}

fn discover_with(environment: &dyn RuntimeEnvironment) -> Option<LinuxXunleiRuntime> {
    let mut candidates = Vec::new();
    if let Some(version) = environment.installed_deb_version() {
        if let Some(candidate) =
            runtime_candidate(LinuxRuntimeSource::Deb, version, environment.deb_root())
        {
            candidates.push(candidate);
        }
    }

    let installed = environment.installed_linyaps_packages();
    let layers_root = environment.linyaps_layers_root();
    if let Ok(entries) = fs::read_dir(layers_root) {
        for entry in entries.flatten() {
            let layer_root = entry.path();
            let Some(metadata) = read_linyaps_metadata(&layer_root) else {
                continue;
            };
            if metadata.id != XUNLEI_PACKAGE_ID
                || !installed.iter().any(|installed| {
                    installed.id == metadata.id && installed.version == metadata.version
                })
            {
                continue;
            }
            if let Some(candidate) = runtime_candidate(
                LinuxRuntimeSource::Linyaps,
                metadata.version,
                layer_root.join("files"),
            ) {
                candidates.push(candidate);
            }
        }
    }

    candidates.into_iter().max_by(compare_runtime_candidates)
}

fn compare_runtime_candidates(left: &LinuxXunleiRuntime, right: &LinuxXunleiRuntime) -> Ordering {
    let left_version = VersionKey::parse(&left.version);
    let right_version = VersionKey::parse(&right.version);
    left_version
        .cmp(&right_version)
        .then_with(|| match (left.source, right.source) {
            (LinuxRuntimeSource::Deb, LinuxRuntimeSource::Linyaps) => Ordering::Greater,
            (LinuxRuntimeSource::Linyaps, LinuxRuntimeSource::Deb) => Ordering::Less,
            _ => Ordering::Equal,
        })
}

fn runtime_candidate(
    source: LinuxRuntimeSource,
    version: String,
    root: PathBuf,
) -> Option<LinuxXunleiRuntime> {
    VersionKey::parse(&version)?;
    let sdk_library = root.join(SDK_LIBRARY_NAME);
    let stat_library = root.join(STAT_LIBRARY_NAME);
    let node_library = root.join(NODE_LIBRARY_NAME);
    let axdl_addon = root.join(AXDL_ADDON_RELATIVE_PATH);
    if !is_x86_64_elf(&sdk_library)
        || !is_x86_64_elf(&stat_library)
        || !is_x86_64_elf(&node_library)
        || !is_x86_64_elf(&axdl_addon)
    {
        return None;
    }
    let stat_sha256 = sha256_file(&stat_library)?;
    let sdk_sha256 = sha256_file(&sdk_library)?;
    let node_sha256 = sha256_file(&node_library)?;
    let axdl_addon_sha256 = sha256_file(&axdl_addon)?;
    Some(LinuxXunleiRuntime {
        source,
        version,
        root,
        node_library,
        stat_library,
        node_sha256,
        stat_sha256,
        sdk_sha256,
        sdk_library,
        axdl_addon_sha256,
        axdl_addon,
    })
}

#[derive(serde::Deserialize)]
struct LinyapsMetadata {
    id: String,
    version: String,
}

fn read_linyaps_metadata(layer_root: &Path) -> Option<LinyapsMetadata> {
    serde_json::from_slice(&fs::read(layer_root.join("info.json")).ok()?).ok()
}

fn is_x86_64_elf(path: &Path) -> bool {
    let mut bytes = [0_u8; 20];
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.read_exact(&mut bytes).is_err() {
        return false;
    }
    bytes[..4] == *b"\x7fELF"
        && bytes[4] == 2
        && bytes[5] == 1
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 62
}

pub(super) fn sha256_file(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Some(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::{
        discover_with, LinuxRuntimeIdentity, LinuxRuntimeSource, LinyapsPackage, RuntimeEnvironment,
    };
    use std::{
        fs,
        path::{Path, PathBuf},
    };
    use tempfile::TempDir;

    const XUNLEI_ID: &str = "com.xunlei.download";

    struct FixtureEnvironment {
        package_version: Option<String>,
        linyaps_packages: Vec<LinyapsPackage>,
        root: TempDir,
    }

    impl FixtureEnvironment {
        fn empty() -> Self {
            Self {
                package_version: None,
                linyaps_packages: Vec::new(),
                root: tempfile::tempdir().unwrap(),
            }
        }

        fn with_deb(version: &str) -> Self {
            let mut environment = Self::empty();
            environment.package_version = Some(version.into());
            write_runtime(&environment.deb_root(), XUNLEI_ID, version, ElfKind::X86_64);
            environment
        }

        fn deb_root(&self) -> PathBuf {
            self.root.path().join("deb").join("files")
        }

        fn layers_root(&self) -> PathBuf {
            self.root.path().join("layers")
        }

        fn add_linyaps(&mut self, layer: &str, id: &str, version: &str, elf: ElfKind) {
            self.linyaps_packages
                .push(LinyapsPackage::new(XUNLEI_ID, version));
            write_runtime(
                &self.layers_root().join(layer).join("files"),
                id,
                version,
                elf,
            );
        }
    }

    impl RuntimeEnvironment for FixtureEnvironment {
        fn installed_deb_version(&self) -> Option<String> {
            self.package_version.clone()
        }

        fn installed_linyaps_packages(&self) -> Vec<LinyapsPackage> {
            self.linyaps_packages.clone()
        }

        fn deb_root(&self) -> PathBuf {
            self.deb_root()
        }

        fn linyaps_layers_root(&self) -> PathBuf {
            self.layers_root()
        }
    }

    #[derive(Clone, Copy)]
    enum ElfKind {
        X86_64,
        WrongArchitecture,
        NotElf,
    }

    fn write_runtime(root: &Path, id: &str, version: &str, elf: ElfKind) {
        fs::create_dir_all(root).unwrap();
        let layer = root.parent().unwrap();
        fs::write(
            layer.join("info.json"),
            serde_json::json!({"id": id, "version": version}).to_string(),
        )
        .unwrap();
        fs::write(root.join("libxl_thunder_sdk.so"), elf_bytes(elf)).unwrap();
        fs::write(root.join("libxl_stat.so"), elf_bytes(elf)).unwrap();
        fs::write(root.join("libnode.so"), elf_bytes(elf)).unwrap();
        let addon = root.join("resources/bin/ThunderKernel.node");
        fs::create_dir_all(addon.parent().unwrap()).unwrap();
        fs::write(addon, elf_bytes(elf)).unwrap();
    }

    fn elf_bytes(kind: ElfKind) -> Vec<u8> {
        if matches!(kind, ElfKind::NotElf) {
            return b"not an elf".to_vec();
        }
        let mut bytes = vec![0_u8; 64];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[6] = 1;
        let machine = if matches!(kind, ElfKind::X86_64) {
            62_u16
        } else {
            3_u16
        };
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    #[test]
    fn deb_runtime_requires_the_installed_package_and_both_valid_elf_libraries() {
        let environment = FixtureEnvironment::with_deb("1.0.0.5");
        let runtime = discover_with(&environment).expect("valid DEB runtime");
        assert_eq!(runtime.source, LinuxRuntimeSource::Deb);
        assert_eq!(runtime.version, "1.0.0.5");

        fs::remove_file(environment.deb_root().join("libxl_stat.so")).unwrap();
        assert!(discover_with(&environment).is_none());
    }

    #[test]
    fn runtime_requires_the_axdl_addon_in_addition_to_its_shared_libraries() {
        let environment = FixtureEnvironment::with_deb("1.0.0.5");
        fs::remove_file(
            environment
                .deb_root()
                .join("resources/bin/ThunderKernel.node"),
        )
        .unwrap();

        assert!(discover_with(&environment).is_none());
    }

    #[test]
    fn runtime_requires_the_packaged_node_library_for_addon_registration() {
        let environment = FixtureEnvironment::with_deb("1.0.0.5");
        fs::remove_file(environment.deb_root().join("libnode.so")).unwrap();

        assert!(discover_with(&environment).is_none());
    }

    #[test]
    fn linyaps_runtime_requires_exact_metadata_id_and_x86_64_elf_libraries() {
        let mut environment = FixtureEnvironment::empty();
        environment.add_linyaps("wrong-id", "com.xunlei.other", "1.0.0.6", ElfKind::X86_64);
        environment.add_linyaps(
            "wrong-elf",
            XUNLEI_ID,
            "1.0.0.6",
            ElfKind::WrongArchitecture,
        );
        environment.add_linyaps("not-elf", XUNLEI_ID, "1.0.0.6", ElfKind::NotElf);
        assert!(discover_with(&environment).is_none());

        environment.add_linyaps("valid", XUNLEI_ID, "1.0.0.6", ElfKind::X86_64);
        let runtime = discover_with(&environment).expect("valid Linyaps runtime");
        assert_eq!(runtime.source, LinuxRuntimeSource::Linyaps);
        assert_eq!(runtime.version, "1.0.0.6");
    }

    #[test]
    fn newest_version_wins_and_deb_wins_an_equal_version_tie() {
        let mut environment = FixtureEnvironment::with_deb("1.0.0.5");
        environment.add_linyaps("newer", XUNLEI_ID, "1.0.0.6", ElfKind::X86_64);
        assert_eq!(
            discover_with(&environment).unwrap().source,
            LinuxRuntimeSource::Linyaps
        );

        environment.package_version = Some("1.0.0.6".into());
        write_runtime(
            &environment.deb_root(),
            XUNLEI_ID,
            "1.0.0.6",
            ElfKind::X86_64,
        );
        assert_eq!(
            discover_with(&environment).unwrap().source,
            LinuxRuntimeSource::Deb
        );
    }

    #[test]
    fn linyaps_persistent_identity_never_includes_the_ephemeral_layer_hash() {
        let mut environment = FixtureEnvironment::empty();
        environment.add_linyaps(
            "4ecb752a3cfeb5e4901fb4d168a7fc1afb661362cd69c564d54d70b5565694e6",
            XUNLEI_ID,
            "1.0.0.6",
            ElfKind::X86_64,
        );

        let runtime = discover_with(&environment).unwrap();
        assert_eq!(
            runtime.identity(),
            LinuxRuntimeIdentity {
                source: LinuxRuntimeSource::Linyaps,
                version: "1.0.0.6".into(),
            }
        );
    }

    #[test]
    fn only_the_observed_library_triple_is_accepted_for_private_axdl_calls() {
        assert!(super::is_supported_axdl_fingerprint(
            "61bd630d03333149c34c6d0f83fc88d316c508d4e6ae9e7389dd556e0a27eb59",
            "6f8fa291b82a287bb690ab326f4f2aa6b1121ebf3891feb5451745458e7e87ee",
            "f6edaccd68eab37a171f53d3cfaafa26f3c38e1494fd849ad8c88c8c622cf277",
            "d6f8a46883185393c744d12e4724240daaf21e35f3639e9523f0a084440683c2",
        ));
        assert!(!super::is_supported_axdl_fingerprint(
            "61bd630d03333149c34c6d0f83fc88d316c508d4e6ae9e7389dd556e0a27eb59",
            "6f8fa291b82a287bb690ab326f4f2aa6b1121ebf3891feb5451745458e7e87ef",
            "f6edaccd68eab37a171f53d3cfaafa26f3c38e1494fd849ad8c88c8c622cf277",
            "d6f8a46883185393c744d12e4724240daaf21e35f3639e9523f0a084440683c2",
        ));
        assert!(super::is_supported_axdl_fingerprint(
            "61bd630d03333149c34c6d0f83fc88d316c508d4e6ae9e7389dd556e0a27eb59",
            "6f8fa291b82a287bb690ab326f4f2aa6b1121ebf3891feb5451745458e7e87ee",
            "f6edaccd68eab37a171f53d3cfaafa26f3c38e1494fd849ad8c88c8c622cf277",
            "153fe8e22f5e1254c10cf4baf6ee4cc47e28ce9ff46acffe1043db28044bf639",
        ));
    }
}
