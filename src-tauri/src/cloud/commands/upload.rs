use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum DirectoryUploadEntry {
    Directory {
        relative_path: PathBuf,
    },
    File {
        relative_path: PathBuf,
        source: PathBuf,
    },
}

#[derive(Clone, Copy)]
struct DirectoryUploadLimits {
    directories: usize,
    files: usize,
    depth: usize,
}

const DIRECTORY_UPLOAD_LIMITS: DirectoryUploadLimits = DirectoryUploadLimits {
    directories: 1_000,
    files: 10_000,
    depth: 128,
};

struct DirectoryUploadPlan {
    root_name: String,
    entries: Vec<DirectoryUploadEntry>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudUploadFailure {
    pub input_index: usize,
    pub item_name: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudUploadOutcome {
    pub uploaded: Vec<CloudFile>,
    pub failures: Vec<CloudUploadFailure>,
}

impl DirectoryUploadEntry {
    fn relative_path(&self) -> &Path {
        match self {
            Self::Directory { relative_path } | Self::File { relative_path, .. } => relative_path,
        }
    }

    #[cfg(test)]
    fn is_directory(&self) -> bool {
        matches!(self, Self::Directory { .. })
    }
}

fn build_directory_upload_plan_with_limits(
    provider: CloudProvider,
    root: &Path,
    control: &crate::cloud::UploadControl,
    limits: DirectoryUploadLimits,
) -> Result<DirectoryUploadPlan, QuarkError> {
    control.check()?;
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的目录".into()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(QuarkError::InvalidInput("请选择可访问的普通目录".into()));
    }
    let root_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| QuarkError::InvalidInput("无法识别上传目录名称".into()))?;
    valid_name(root_name)?;

    let mut entries = Vec::new();
    let mut directories = 1_usize;
    let mut files = 0_usize;
    let mut pending = vec![(root.to_path_buf(), PathBuf::new(), false)];
    while let Some((path, relative_path, emit_directory)) = pending.pop() {
        control.check()?;
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| QuarkError::InvalidInput("无法读取目录中的项目".into()))?;
        if metadata.file_type().is_symlink() {
            return Err(QuarkError::InvalidInput(
                "上传目录不能包含符号链接或目录联接".into(),
            ));
        }
        let depth = relative_path.components().count();
        if depth > limits.depth {
            return Err(QuarkError::InvalidInput("上传目录层级过深".into()));
        }
        if metadata.is_file() {
            files = files.saturating_add(1);
            if files > limits.files {
                return Err(QuarkError::InvalidInput("上传目录包含过多文件".into()));
            }
            validate_upload_file(provider, &path, &metadata)?;
            entries.push(DirectoryUploadEntry::File {
                relative_path,
                source: path,
            });
            continue;
        }
        if !metadata.is_dir() {
            return Err(QuarkError::InvalidInput(
                "上传目录只能包含普通文件和目录".into(),
            ));
        }
        if emit_directory {
            directories = directories.saturating_add(1);
            if directories > limits.directories {
                return Err(QuarkError::InvalidInput("上传目录包含过多子目录".into()));
            }
            let name = relative_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| QuarkError::InvalidInput("目录名称无效".into()))?;
            valid_name(name)?;
            entries.push(DirectoryUploadEntry::Directory {
                relative_path: relative_path.clone(),
            });
        }
        let mut children = std::fs::read_dir(&path)
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的目录".into()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| QuarkError::InvalidInput("无法读取选中的目录".into()))?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children.into_iter().rev() {
            control.check()?;
            let child_path = child.path();
            let child_relative = child_path
                .strip_prefix(root)
                .map_err(|_| QuarkError::InvalidInput("目录路径无效".into()))?
                .to_path_buf();
            pending.push((child_path, child_relative, true));
        }
    }
    Ok(DirectoryUploadPlan {
        root_name: root_name.to_owned(),
        entries,
    })
}

fn collect_directory_upload_entries(root: &Path) -> Result<Vec<DirectoryUploadEntry>, QuarkError> {
    Ok(build_directory_upload_plan_with_limits(
        CloudProvider::Xunlei,
        root,
        &crate::cloud::UploadControl::default(),
        DIRECTORY_UPLOAD_LIMITS,
    )?
    .entries)
}

fn validate_upload_file(
    provider: CloudProvider,
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), QuarkError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| QuarkError::InvalidInput("文件名无效".into()))?;
    valid_name(name)?;
    if provider == CloudProvider::Baidu && metadata.len() == 0 {
        return Err(QuarkError::InvalidInput(
            "百度网盘暂不支持上传空文件".into(),
        ));
    }
    Ok(())
}

fn ensure_upload_root_available(
    destination: &CloudDirectory,
    root_name: &str,
) -> Result<(), QuarkError> {
    if destination.items.iter().any(|item| item.name == root_name) {
        return Err(QuarkError::InvalidInput(
            "目标目录已存在同名项目，请先重命名本地目录或远端项目".into(),
        ));
    }
    Ok(())
}

pub(crate) fn select_cloud_upload_paths(
    descriptor: &ProviderDescriptor,
    paths: Option<Vec<String>>,
    pick_files: impl FnOnce(String) -> Result<Vec<PathBuf>, QuarkError>,
) -> Result<Vec<PathBuf>, QuarkError> {
    match paths {
        Some(paths) => Ok(paths.into_iter().map(PathBuf::from).collect()),
        None => pick_files(format!(
            "选择上传到 {} 的文件",
            descriptor.upload_display_name
        )),
    }
}

fn upload_directory_tree(
    state: &CloudState,
    http: &(impl CloudHttpTransport + CloudUploadTransport),
    provider: CloudProvider,
    parent_id: &str,
    root: &Path,
    control: &crate::cloud::UploadControl,
) -> Result<Vec<CloudFile>, QuarkError> {
    let plan =
        build_directory_upload_plan_with_limits(provider, root, control, DIRECTORY_UPLOAD_LIMITS)?;
    control.check()?;
    let destination = state.with_upload_session(provider, http, |store| {
        ProviderRegistry::global()
            .driver(provider, store, http)
            .list_directory(parent_id)
    })?;
    ensure_upload_root_available(&destination, &plan.root_name)?;
    let create_directory = |parent_id: &str, name: &str| {
        state.with_upload_session(provider, http, |store| {
            ProviderRegistry::global()
                .driver(provider, store, http)
                .create_directory(parent_id, name)
        })
    };
    execute_directory_upload_plan(
        parent_id,
        plan,
        || control.check(),
        create_directory,
        |remote_parent, source| {
            ProviderRegistry::global()
                .driver(provider, &state.store, http)
                .upload(state, http, remote_parent, source)
        },
        |root_id| {
            let ids = vec![root_id.to_owned()];
            state.with_upload_session(provider, http, |store| {
                ProviderRegistry::global()
                    .driver(provider, store, http)
                    .trash(&ids)
            })
        },
    )
}

fn execute_directory_upload_plan(
    parent_id: &str,
    plan: DirectoryUploadPlan,
    mut check_control: impl FnMut() -> Result<(), QuarkError>,
    mut create_directory: impl FnMut(&str, &str) -> Result<CloudFile, QuarkError>,
    mut upload_file: impl FnMut(&str, &Path) -> Result<CloudFile, QuarkError>,
    mut trash_root: impl FnMut(&str) -> Result<(), QuarkError>,
) -> Result<Vec<CloudFile>, QuarkError> {
    check_control()?;
    let uploaded_root = create_directory(parent_id, &plan.root_name)?;
    let mut results = vec![uploaded_root.clone()];
    let root_id = uploaded_root.id.clone();
    let operation = (|| {
        let mut remote_directories = HashMap::from([(PathBuf::new(), root_id.clone())]);
        for entry in plan.entries {
            check_control()?;
            let relative_parent = entry
                .relative_path()
                .parent()
                .unwrap_or_else(|| Path::new(""));
            let remote_parent = remote_directories
                .get(relative_parent)
                .cloned()
                .ok_or(QuarkError::InvalidResponse)?;
            match entry {
                DirectoryUploadEntry::Directory { relative_path } => {
                    let name = relative_path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or_else(|| QuarkError::InvalidInput("目录名称无效".into()))?;
                    let created = create_directory(&remote_parent, name)?;
                    remote_directories.insert(relative_path, created.id.clone());
                    results.push(created);
                }
                DirectoryUploadEntry::File { source, .. } => {
                    results.push(upload_file(&remote_parent, &source)?);
                }
            }
        }
        Ok(results)
    })();
    match operation {
        Ok(results) => Ok(results),
        Err(error) => match trash_root(&root_id) {
            Ok(()) => Err(error),
            Err(_) => Err(QuarkError::Provider(
                "目录上传失败，且无法自动回收本次新建的远端目录；请刷新后手动处理".into(),
            )),
        },
    }
}

fn upload_selected_path(
    state: &CloudState,
    provider: CloudProvider,
    parent_id: &str,
    path: &Path,
    control: crate::cloud::UploadControl,
) -> Result<Vec<CloudFile>, QuarkError> {
    control.check()?;
    let transport = ReqwestTransport::new()?;
    let http = crate::cloud::http::ControlledUploadHttp {
        inner: &transport,
        control: control.clone(),
    };
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| QuarkError::InvalidInput("无法读取选中的上传项目".into()))?;
    if metadata.file_type().is_symlink() {
        return Err(QuarkError::InvalidInput(
            "不能上传符号链接或目录联接".into(),
        ));
    }
    if metadata.is_dir() {
        upload_directory_tree(state, &http, provider, parent_id, path, &control)
    } else if metadata.is_file() {
        validate_upload_file(provider, path, &metadata)?;
        control.check()?;
        Ok(vec![ProviderRegistry::global()
            .driver(provider, &state.store, &http)
            .upload(state, &http, parent_id, path)?])
    } else {
        Err(QuarkError::InvalidInput("只能上传普通文件或目录".into()))
    }
}

fn upload_error_code(error: &QuarkError) -> &'static str {
    match error {
        QuarkError::NotAuthorized => "not_authorized",
        QuarkError::AuthorizationExpired => "credentials_expired",
        QuarkError::SessionRenewedRetryOperation => "session_renewed_retry_operation",
        QuarkError::WebAuthorizationRequired | QuarkError::CaptchaRequired => "web_auth_required",
        QuarkError::PikpakRegionalMembershipRestricted => "pikpak_regional_membership_restricted",
        QuarkError::AuthorizationPending => "authorization_pending",
        QuarkError::InvalidResponse => "invalid_response",
        QuarkError::Storage => "credential_storage_error",
        QuarkError::Transport(_) => "network_error",
        QuarkError::Provider(_) => "provider_error",
        QuarkError::InvalidInput(_) => "invalid_input",
        QuarkError::UploadUnavailable | QuarkError::ProviderUnsupported => "provider_unsupported",
    }
}

fn summarize_upload_batches(
    batches: Vec<(usize, String, Result<Vec<CloudFile>, QuarkError>)>,
) -> CloudUploadOutcome {
    let mut outcome = CloudUploadOutcome {
        uploaded: Vec::new(),
        failures: Vec::new(),
    };
    for (input_index, item_name, batch) in batches {
        match batch {
            Ok(files) => outcome.uploaded.extend(files),
            Err(error) => outcome.failures.push(CloudUploadFailure {
                input_index,
                item_name,
                code: upload_error_code(&error),
                message: error.to_string(),
            }),
        }
    }
    outcome
}

async fn upload_selected_paths(
    state: Arc<CloudState>,
    descriptor: &'static ProviderDescriptor,
    provider: CloudProvider,
    parent_id: String,
    paths: Vec<PathBuf>,
) -> Result<CloudUploadOutcome, QuarkError> {
    let inputs = paths
        .into_iter()
        .map(|path| {
            let item_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "上传项目".into());
            (item_name, path)
        })
        .collect();
    let batches = crate::cloud::schedule_uploads(
        inputs,
        descriptor.max_upload_concurrency,
        crate::cloud::UploadControl::default(),
        move |(item_name, path), control| {
            Ok((
                item_name,
                upload_selected_path(&state, provider, &parent_id, &path, control),
            ))
        },
    )
    .await;
    let batches = batches
        .into_iter()
        .enumerate()
        .map(|(input_index, batch)| {
            let (item_name, result) = batch.unwrap_or_else(|error| ("上传项目".into(), Err(error)));
            (input_index, item_name, result)
        })
        .collect();
    Ok(summarize_upload_batches(batches))
}

#[tauri::command]
pub async fn cloud_upload_files(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    paths: Option<Vec<String>>,
) -> Result<CloudUploadOutcome, QuarkError> {
    let picker = app.clone();
    let descriptor = ProviderRegistry::global().get(provider);
    // The native picker may stay open indefinitely; it must hold no session lock.
    let paths = tauri::async_runtime::spawn_blocking(move || {
        select_cloud_upload_paths(descriptor, paths, |title| {
            picker
                .dialog()
                .file()
                .set_title(title)
                .blocking_pick_files()
                .unwrap_or_default()
                .into_iter()
                .map(|path| {
                    path.into_path()
                        .map_err(|_| QuarkError::InvalidInput("无法访问选中的文件".into()))
                })
                .collect()
        })
    })
    .await
    .map_err(|_| QuarkError::InvalidInput("文件选择中断".into()))??;
    if paths.is_empty() {
        return Ok(CloudUploadOutcome {
            uploaded: Vec::new(),
            failures: Vec::new(),
        });
    }
    upload_selected_paths(
        app.state::<Arc<CloudState>>().inner().clone(),
        descriptor,
        provider,
        parent_id,
        paths,
    )
    .await
}

#[tauri::command]
pub async fn cloud_upload_directory(
    app: AppHandle,
    provider: CloudProvider,
    parent_id: String,
    path: Option<String>,
) -> Result<CloudUploadOutcome, QuarkError> {
    let descriptor = ProviderRegistry::global().get(provider);
    let selected = match path {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            let picker = app.clone();
            let title = format!("选择上传到 {} 的目录", descriptor.upload_display_name);
            tauri::async_runtime::spawn_blocking(move || {
                picker
                    .dialog()
                    .file()
                    .set_title(title)
                    .blocking_pick_folder()
                    .map(|path| {
                        path.into_path()
                            .map_err(|_| QuarkError::InvalidInput("无法访问选中的目录".into()))
                    })
                    .transpose()
            })
            .await
            .map_err(|_| QuarkError::InvalidInput("目录选择中断".into()))??
        }
    };
    let Some(path) = selected else {
        return Ok(CloudUploadOutcome {
            uploaded: Vec::new(),
            failures: Vec::new(),
        });
    };
    upload_selected_paths(
        app.state::<Arc<CloudState>>().inner().clone(),
        descriptor,
        provider,
        parent_id,
        vec![path],
    )
    .await
}

impl CloudState {
    pub(crate) fn with_upload_session<T>(
        &self,
        provider: CloudProvider,
        http: &impl CloudHttpTransport,
        mut work: impl FnMut(&crate::cloud::driver::StoreRef<'_>) -> Result<T, CloudError>,
    ) -> Result<T, CloudError> {
        self.auth_supervisor.run_checked(
            &self.store,
            provider,
            SessionWorkKind::Mutation,
            |snapshot| work(&crate::cloud::driver::StoreRef(snapshot)),
            |genuine| {
                auth_supervisor::recover_in_order(
                    provider,
                    &self.store,
                    ProviderRegistry::global()
                        .driver(provider, &self.store, http)
                        .auth(),
                    genuine,
                    || Ok(false),
                )
            },
            || http.check_transfer(),
        )
    }
    pub fn upload_pikpak_file(
        &self,
        http: &(impl CloudHttpTransport + CloudUploadTransport),
        parent: &str,
        path: &Path,
    ) -> Result<CloudFile, QuarkError> {
        self.with_upload_session(CloudProvider::Pikpak, http, |store| {
            PikPakClient::new(http, store).upload_file(parent, path)
        })
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    fn file(id: &str, parent_id: &str, name: &str, kind: CloudFileKind) -> CloudFile {
        CloudFile {
            id: id.into(),
            parent_id: parent_id.into(),
            name: name.into(),
            kind,
            size: None,
            modified_at: None,
        }
    }

    #[test]
    fn directory_upload_plan_preserves_empty_and_nested_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("empty")).unwrap();
        std::fs::create_dir_all(root.path().join("nested").join("child")).unwrap();
        std::fs::write(root.path().join("top.txt"), b"top").unwrap();
        std::fs::write(
            root.path().join("nested").join("child").join("leaf.txt"),
            b"leaf",
        )
        .unwrap();

        let plan = collect_directory_upload_entries(root.path()).unwrap();
        let relative = plan
            .iter()
            .map(|entry| entry.relative_path().to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();

        assert_eq!(
            relative,
            [
                "empty",
                "nested",
                "nested/child",
                "nested/child/leaf.txt",
                "top.txt"
            ]
        );
        assert!(plan[0].is_directory());
        assert!(plan[2].is_directory());
        assert!(!plan[3].is_directory());
    }

    #[test]
    fn directory_upload_plan_checks_cancellation_and_bounds_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.txt"), b"a").unwrap();
        std::fs::write(root.path().join("b.txt"), b"b").unwrap();
        let control = crate::cloud::UploadControl::default();

        assert!(matches!(
            build_directory_upload_plan_with_limits(
                CloudProvider::Xunlei,
                root.path(),
                &control,
                DirectoryUploadLimits { directories: 10, files: 1, depth: 10 },
            ),
            Err(QuarkError::InvalidInput(message)) if message == "上传目录包含过多文件"
        ));

        control.cancel();
        assert!(matches!(
            build_directory_upload_plan_with_limits(
                CloudProvider::Xunlei,
                root.path(),
                &control,
                DirectoryUploadLimits { directories: 10, files: 10, depth: 10 },
            ),
            Err(QuarkError::InvalidInput(message)) if message == "上传已取消"
        ));
    }

    #[test]
    fn baidu_empty_file_is_rejected_during_preflight() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("empty.txt"), []).unwrap();

        assert!(matches!(
            build_directory_upload_plan_with_limits(
                CloudProvider::Baidu,
                root.path(),
                &crate::cloud::UploadControl::default(),
                DirectoryUploadLimits { directories: 10, files: 10, depth: 10 },
            ),
            Err(QuarkError::InvalidInput(message)) if message == "百度网盘暂不支持上传空文件"
        ));
    }

    #[test]
    fn directory_upload_failure_trashes_only_the_new_root() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("leaf.txt");
        std::fs::write(&source, b"leaf").unwrap();
        let plan = DirectoryUploadPlan {
            root_name: "fixture".into(),
            entries: vec![DirectoryUploadEntry::File {
                relative_path: "leaf.txt".into(),
                source,
            }],
        };
        let mut trashed = Vec::new();

        let result = execute_directory_upload_plan(
            "parent",
            plan,
            || Ok(()),
            |parent, name| Ok(file("new-root", parent, name, CloudFileKind::Folder)),
            |_, _| Err(QuarkError::Transport("offline".into())),
            |root_id| {
                trashed.push(root_id.to_owned());
                Ok(())
            },
        );

        assert!(matches!(result, Err(QuarkError::Transport(_))));
        assert_eq!(trashed, ["new-root"]);
    }

    #[test]
    fn directory_upload_cancellation_stops_before_the_next_mutation_and_rolls_back() {
        let plan = DirectoryUploadPlan {
            root_name: "fixture".into(),
            entries: vec![DirectoryUploadEntry::Directory {
                relative_path: "empty".into(),
            }],
        };
        let mut checks = 0;
        let mut creates = Vec::new();
        let mut trashed = Vec::new();

        let result = execute_directory_upload_plan(
            "parent",
            plan,
            || {
                checks += 1;
                if checks == 2 {
                    Err(QuarkError::InvalidInput("上传已取消".into()))
                } else {
                    Ok(())
                }
            },
            |parent, name| {
                creates.push((parent.to_owned(), name.to_owned()));
                Ok(file("new-root", parent, name, CloudFileKind::Folder))
            },
            |_, _| unreachable!(),
            |root_id| {
                trashed.push(root_id.to_owned());
                Ok(())
            },
        );

        assert!(matches!(
            result,
            Err(QuarkError::InvalidInput(message)) if message == "上传已取消"
        ));
        assert_eq!(creates, [("parent".into(), "fixture".into())]);
        assert_eq!(trashed, ["new-root"]);
    }

    #[test]
    fn directory_upload_rejects_a_conflicting_remote_name() {
        let destination = CloudDirectory {
            id: "parent".into(),
            items: vec![file("existing", "parent", "fixture", CloudFileKind::Folder)],
        };

        assert!(matches!(
            ensure_upload_root_available(&destination, "fixture"),
            Err(QuarkError::InvalidInput(message)) if message.contains("已存在同名项目")
        ));
    }

    #[test]
    fn upload_batch_outcome_keeps_successes_and_reports_failures() {
        let outcome = summarize_upload_batches(vec![
            (
                0,
                "a.txt".into(),
                Ok(vec![file("a", "root", "a.txt", CloudFileKind::File)]),
            ),
            (
                1,
                "b.txt".into(),
                Err(QuarkError::Transport("offline".into())),
            ),
        ]);

        assert_eq!(outcome.uploaded.len(), 1);
        assert_eq!(outcome.uploaded[0].id, "a");
        assert_eq!(outcome.failures.len(), 1);
        assert_eq!(outcome.failures[0].input_index, 1);
        assert_eq!(outcome.failures[0].item_name, "b.txt");
        assert_eq!(outcome.failures[0].code, "network_error");
    }
}
