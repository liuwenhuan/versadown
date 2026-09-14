use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalUploadEntry {
    path: String,
    name: String,
    kind: String,
    size: Option<u64>,
    modified_at: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalUploadListing {
    path: String,
    parent_path: Option<String>,
    roots: Vec<LocalUploadEntry>,
    entries: Vec<LocalUploadEntry>,
    double_click_time_ms: u32,
}

#[tauri::command]
pub async fn local_upload_entries(path: Option<String>) -> Result<LocalUploadListing, String> {
    tauri::async_runtime::spawn_blocking(move || list_local_entries(path))
        .await
        .map_err(|error| format!("读取本地目录的任务失败：{error}"))?
}

fn list_local_entries(path: Option<String>) -> Result<LocalUploadListing, String> {
    let requested = match path {
        Some(path) if path.trim().is_empty() => return Err("请输入本地目录路径".into()),
        Some(path) => PathBuf::from(path),
        None => std::env::home_dir().ok_or("无法确定用户目录，请输入本地目录路径")?,
    };
    let metadata = fs::symlink_metadata(&requested)
        .map_err(|error| format!("无法读取目录“{}”：{error}", requested.display()))?;
    if is_link(&metadata) {
        return Err(format!("不支持选择链接目录“{}”", requested.display()));
    }
    if !metadata.is_dir() {
        return Err(format!("路径不是目录：“{}”", requested.display()));
    }
    let directory = fs::canonicalize(&requested)
        .map_err(|error| format!("无法解析目录“{}”：{error}", requested.display()))?;
    let path = display_path(&directory)?;
    let parent_path = directory.parent().map(display_path).transpose()?;
    let reader =
        fs::read_dir(&directory).map_err(|error| format!("无法打开目录“{path}”：{error}"))?;
    let mut entries = Vec::new();
    let mut skipped_error = None;
    for item in reader {
        let item = item.map_err(|error| format!("读取目录“{path}”失败：{error}"))?;
        let metadata = match fs::symlink_metadata(item.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                skipped_error = Some(error.to_string());
                continue;
            }
        };
        if is_link(&metadata) || (!metadata.is_file() && !metadata.is_dir()) {
            continue;
        }
        let name = match item.file_name().into_string() {
            Ok(name) => name,
            Err(_) => {
                skipped_error = Some("文件名包含无法显示的字符".into());
                continue;
            }
        };
        // read_dir names are single components; joining the canonical directory
        // yields absolute paths without following links or reading file content.
        entries.push(LocalUploadEntry {
            path: display_path(&directory.join(&name))?,
            name,
            kind: if metadata.is_dir() {
                "directory"
            } else {
                "file"
            }
            .into(),
            size: metadata.is_file().then(|| metadata.len()),
            modified_at: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|duration| u64::try_from(duration.as_millis()).ok()),
        });
    }
    if entries.is_empty() {
        if let Some(error) = skipped_error {
            return Err(format!("无法读取目录“{path}”中的条目：{error}"));
        }
    }
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(LocalUploadListing {
        path,
        parent_path,
        roots: local_roots(),
        entries,
        double_click_time_ms: {
            #[cfg(windows)]
            {
                // SAFETY: reads a system preference without arguments or pointers.
                unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
            }
            #[cfg(not(windows))]
            {
                500
            }
        },
    })
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes symlinks, junctions and other reparse points (e.g. placeholders).
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn display_path(path: &Path) -> Result<String, String> {
    let path = path.to_str().ok_or("本地路径包含无法显示的字符")?;
    #[cfg(windows)]
    {
        if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
            return Ok(format!(r"\\{unc}"));
        }
        if let Some(drive) = path.strip_prefix(r"\\?\") {
            return Ok(drive.into());
        }
    }
    Ok(path.into())
}

fn local_roots() -> Vec<LocalUploadEntry> {
    #[cfg(windows)]
    {
        // Query the system's drive-letter table without touching any volumes:
        // probing metadata here would stall local browsing on disconnected shares.
        // SAFETY: GetLogicalDrives has no arguments or pointer preconditions.
        let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
        windows_roots_from_mask(mask)
    }
    #[cfg(not(windows))]
    {
        vec![LocalUploadEntry {
            name: "/".into(),
            path: "/".into(),
            kind: "directory".into(),
            size: None,
            modified_at: None,
        }]
    }
}

#[cfg(windows)]
fn windows_roots_from_mask(mask: u32) -> Vec<LocalUploadEntry> {
    (0..26)
        .filter(|index| mask & (1 << index) != 0)
        .map(|index| {
            let path = format!("{}:\\", (b'A' + index) as char);
            LocalUploadEntry {
                name: path.clone(),
                path,
                kind: "directory".into(),
                size: None,
                modified_at: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    fn listing(path: &Path) -> Result<LocalUploadListing, String> {
        list_local_entries(Some(path.to_str().unwrap().to_owned()))
    }

    #[test]
    fn serializes_file_sizes_and_modification_times_without_directory_totals() {
        use std::time::{Duration, UNIX_EPOCH};
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("folder")).unwrap();
        fs::write(temp.path().join("file.txt"), b"hello").unwrap();
        fs::write(temp.path().join("empty.txt"), b"").unwrap();
        let file = fs::File::options()
            .write(true)
            .open(temp.path().join("file.txt"))
            .unwrap();
        file.set_times(
            fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        )
        .unwrap();
        let json = serde_json::to_value(listing(temp.path()).unwrap()).unwrap();
        let entries = json["entries"].as_array().unwrap();
        let named = |name: &str| entries.iter().find(|entry| entry["name"] == name).unwrap();
        assert_eq!(named("file.txt")["size"], 5);
        assert_eq!(named("empty.txt")["size"], 0);
        assert_eq!(named("file.txt")["modifiedAt"], 1_700_000_000_000u64);
        assert!(named("folder")["size"].is_null());
        assert!(named("folder")["modifiedAt"].is_number());
    }

    #[cfg(windows)]
    #[test]
    fn zero_drive_mask_returns_no_roots() {
        assert!(windows_roots_from_mask(0).is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn drive_mask_maps_a_c_and_z_in_letter_order() {
        let roots = windows_roots_from_mask(0x0200_0005);
        let rows: Vec<_> = roots
            .iter()
            .map(|entry| {
                (
                    entry.path.as_str(),
                    entry.name.as_str(),
                    entry.kind.as_str(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                ("A:\\", "A:\\", "directory"),
                ("C:\\", "C:\\", "directory"),
                ("Z:\\", "Z:\\", "directory")
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn drive_mask_ignores_bits_above_z() {
        assert!(windows_roots_from_mask(0xfc00_0000).is_empty());
        let roots = windows_roots_from_mask(0xfc00_0004);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].path, "C:\\");
    }

    #[test]
    fn lists_files_and_empty_directories_in_directory_first_name_order() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.txt"), b"file content").unwrap();
        fs::write(temp.path().join("Z.txt"), b"other content").unwrap();
        fs::create_dir(temp.path().join("beta")).unwrap();
        fs::create_dir(temp.path().join("Alpha")).unwrap();
        let result = listing(temp.path()).unwrap();
        let rows: Vec<_> = result
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Alpha", "directory"),
                ("beta", "directory"),
                ("a.txt", "file"),
                ("Z.txt", "file")
            ]
        );
        assert!(result
            .entries
            .iter()
            .all(|entry| Path::new(&entry.path).is_absolute()));
        assert!(listing(&temp.path().join("Alpha"))
            .unwrap()
            .entries
            .is_empty());
        assert_eq!(
            fs::read(temp.path().join("a.txt")).unwrap(),
            b"file content"
        );
    }

    #[test]
    fn normalizes_navigation_path_and_returns_its_parent() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        fs::create_dir(&child).unwrap();
        let base = listing(temp.path()).unwrap();
        let nested = listing(&child.join(".")).unwrap();
        assert_eq!(
            nested.path,
            Path::new(&base.path).join("child").to_str().unwrap()
        );
        assert_eq!(nested.parent_path.as_deref(), Some(base.path.as_str()));
        assert_eq!(listing(&child.join("..")).unwrap().path, base.path);
        assert!(!base.path.starts_with(r"\\?\"));
        assert!(base
            .roots
            .iter()
            .all(|root| root.kind == "directory" && Path::new(&root.path).is_absolute()));
        assert!(!base.roots.is_empty());
    }

    #[test]
    fn rejects_missing_paths_and_files_as_directories() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("file.txt"), b"x").unwrap();
        let missing = listing(&temp.path().join("missing")).unwrap_err();
        assert!(missing.contains("missing"));
        assert!(listing(&temp.path().join("file.txt"))
            .unwrap_err()
            .contains("目录"));
    }

    #[test]
    fn uses_home_directory_when_no_path_is_given() {
        let home = std::env::home_dir().expect("test account home directory");
        // Sandboxed test accounts may not allow listing their home directory;
        // default navigation must select it and preserve the same access error.
        assert_eq!(
            list_local_entries(None).map(|listing| listing.path),
            listing(&home).map(|listing| listing.path)
        );
    }

    #[cfg(windows)]
    #[test]
    fn excludes_junctions_and_rejects_opening_them() {
        use std::os::windows::process::CommandExt;
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let junction = temp.path().join("junction");
        fs::create_dir(&target).unwrap();
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .creation_flags(0x08000000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result = listing(temp.path()).unwrap();
        assert_eq!(
            result
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["target"]
        );
        assert!(listing(&junction).unwrap_err().contains("链接"));
    }

    #[cfg(unix)]
    #[test]
    fn excludes_symlinks_including_broken_links() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("file.txt"), b"x").unwrap();
        symlink(temp.path().join("file.txt"), temp.path().join("link")).unwrap();
        symlink(temp.path().join("missing"), temp.path().join("broken")).unwrap();
        let result = listing(temp.path()).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].name, "file.txt");
    }
}
