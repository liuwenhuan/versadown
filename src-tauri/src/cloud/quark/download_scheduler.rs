use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};

use crate::{
    aria2::{Aria2Api, Aria2Options, Aria2RpcClient, RpcSecret},
    cloud::CloudProvider,
    commands::AppState,
    DownloadTask, StoreError, TaskStatus, TaskStore,
};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

pub(crate) const MIN_PARALLEL_REMAINING_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum QuarkBudgetScope {
    Directory(Uuid),
    Task(Uuid),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuarkBudgetMember {
    pub(crate) order: u32,
    pub(crate) task_id: Option<Uuid>,
    pub(crate) has_current_gid: bool,
    pub(crate) reserves_connection: bool,
    pub(crate) remaining_bytes: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct QuarkConnectionTarget {
    pub(crate) task_id: Uuid,
    pub(crate) gid: String,
    pub(crate) connections: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AppliedConnection {
    gid: String,
    connections: u8,
}

#[async_trait::async_trait]
pub(crate) trait QuarkConnectionOptionClient: Send + Sync {
    async fn change_connections(&self, gid: &str, options: Aria2Options) -> Result<(), ()>;
}

#[async_trait::async_trait]
impl QuarkConnectionOptionClient for Aria2RpcClient {
    async fn change_connections(&self, gid: &str, options: Aria2Options) -> Result<(), ()> {
        self.change_option(gid, options)
            .await
            .map(|_| ())
            .map_err(|_| ())
    }
}

#[derive(Default)]
pub(crate) struct QuarkDownloadCoordinator {
    running: AtomicBool,
    activation_generation: AtomicU64,
    applied: Mutex<BTreeMap<Uuid, AppliedConnection>>,
}

pub(crate) fn coordinator() -> &'static QuarkDownloadCoordinator {
    static COORDINATOR: OnceLock<QuarkDownloadCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(QuarkDownloadCoordinator::default)
}

impl QuarkDownloadCoordinator {
    /// Starts a plugin-local reconciliation loop. The loop has no persistent
    /// task state: every tick rebuilds its plan from the durable store.
    pub(crate) fn activate(&'static self, app: AppHandle) {
        self.activation_generation.fetch_add(1, Ordering::AcqRel);
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        tauri::async_runtime::spawn(async move {
            loop {
                let observed_activation = self.activation_generation.load(Ordering::Acquire);
                if !self.reconcile_once(&app).await {
                    if !self.stop_or_reclaim_after_activation(observed_activation) {
                        break;
                    }
                    continue;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        });
    }

    fn stop_or_reclaim_after_activation(&self, observed_activation: u64) -> bool {
        self.running.store(false, Ordering::Release);
        self.activation_generation.load(Ordering::Acquire) != observed_activation
            && self
                .running
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
    }

    async fn reconcile_once(&self, app: &AppHandle) -> bool {
        let state = app.state::<AppState>();
        let settings = match state.settings.lock() {
            Ok(settings) => settings.aria2.clone(),
            Err(_) => return true,
        };
        let targets = match connection_targets(&state.store, settings.max_connections_per_server) {
            Ok(targets) => targets,
            Err(_) => return true,
        };
        let expected = targets
            .iter()
            .map(|target| (target.task_id, target.gid.as_str()))
            .collect::<BTreeMap<_, _>>();
        let changes = {
            let mut applied = self
                .applied
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            applied.retain(|task_id, current| {
                expected.get(task_id).is_some_and(|gid| *gid == current.gid)
            });
            pending_connection_changes(&applied, &targets)
        };
        if targets.is_empty() {
            return false;
        }
        let secret = match RpcSecret::from_token(&settings.rpc_token) {
            Ok(secret) => secret,
            Err(_) => return true,
        };
        let endpoint = format!("http://127.0.0.1:{}/jsonrpc", settings.rpc_port);
        let client = match Aria2RpcClient::new(endpoint, secret) {
            Ok(client) => client,
            Err(_) => return true,
        };
        for target in changes {
            if apply_connection_target(&client, &state.store, &target).await {
                self.applied
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .insert(
                        target.task_id,
                        AppliedConnection {
                            gid: target.gid,
                            connections: target.connections,
                        },
                    );
            }
        }
        true
    }
}

impl QuarkBudgetMember {
    fn is_parallel_eligible(&self) -> bool {
        self.reserves_connection
            && self.has_current_gid
            && self.task_id.is_some()
            && self
                .remaining_bytes
                .is_some_and(|remaining| remaining >= MIN_PARALLEL_REMAINING_BYTES)
    }
}

/// Allocates connection targets for one Quark task.  Directory entries that
/// do not have a GID yet still reserve a base connection so an earlier file
/// cannot consume the later file's slot with Range segments.
pub(crate) fn allocate_connections(
    budget: u8,
    mut members: Vec<QuarkBudgetMember>,
) -> BTreeMap<Uuid, u8> {
    let budget = usize::from(budget.max(1));
    members.sort_by_key(|member| member.order);

    let reserved = members
        .iter()
        .filter(|member| member.reserves_connection)
        .count()
        .min(budget);
    let mut allocation = members
        .iter()
        .filter(|member| {
            member.reserves_connection && member.has_current_gid && member.task_id.is_some()
        })
        .filter_map(|member| member.task_id.map(|task_id| (task_id, 1u8)))
        .collect::<BTreeMap<_, _>>();
    let eligible = members
        .iter()
        .filter(|member| member.is_parallel_eligible())
        .filter_map(|member| member.task_id)
        .collect::<Vec<_>>();
    let mut remaining = budget.saturating_sub(reserved);

    while remaining > 0 && !eligible.is_empty() {
        for task_id in &eligible {
            if remaining == 0 {
                break;
            }
            if let Some(connections) = allocation.get_mut(task_id) {
                *connections = (*connections).saturating_add(1);
                remaining -= 1;
            }
        }
    }
    allocation
}

pub(crate) fn allocation_for_scope(
    store: &TaskStore,
    scope: QuarkBudgetScope,
    budget: u8,
) -> Result<BTreeMap<Uuid, u8>, StoreError> {
    Ok(allocate_connections(
        budget,
        members_for_scope(store, scope)?,
    ))
}

pub(crate) fn initial_connections_for_task(
    store: &TaskStore,
    task_id: Uuid,
    budget: u8,
) -> Result<u8, StoreError> {
    let task = store.find(task_id)?;
    if !is_quark_source(&task.source) {
        return Ok(1);
    }
    let group = store.cloud_directory_group_for_task(task_id)?;
    if group.is_some() && task.aria_gid.is_none() {
        // The durable directory snapshot already reserves this file's base
        // connection. Start its new GID at one, then rebalance after aria2 has
        // accepted it; otherwise its final target could briefly overlap with
        // the targets currently held by earlier files.
        return Ok(1);
    }
    let scope = group
        .map(|group| QuarkBudgetScope::Directory(group.id))
        .unwrap_or(QuarkBudgetScope::Task(task_id));
    let mut members = members_for_scope(store, scope)?;
    if let Some(member) = members
        .iter_mut()
        .find(|member| member.task_id == Some(task_id))
    {
        member.has_current_gid = true;
    }
    Ok(allocate_connections(budget, members)
        .get(&task_id)
        .copied()
        .unwrap_or(1))
}

fn members_for_scope(
    store: &TaskStore,
    scope: QuarkBudgetScope,
) -> Result<Vec<QuarkBudgetMember>, StoreError> {
    match scope {
        QuarkBudgetScope::Directory(group_id) => {
            let Some(group) = store.cloud_directory_group(group_id)? else {
                return Ok(Vec::new());
            };
            if group.provider != CloudProvider::Quark {
                return Ok(Vec::new());
            }
            store
                .cloud_directory_group_items(group_id)?
                .into_iter()
                .map(|item| match item.task_id {
                    Some(task_id) => {
                        let task = store.find(task_id)?;
                        Ok(member_from_task(
                            item.item_index,
                            Some(task_id),
                            &task,
                            item.failure_code.is_none(),
                            item.size,
                        ))
                    }
                    None => Ok(QuarkBudgetMember {
                        order: item.item_index,
                        task_id: None,
                        has_current_gid: false,
                        reserves_connection: item.failure_code.is_none(),
                        remaining_bytes: item.size,
                    }),
                })
                .collect::<Result<Vec<_>, StoreError>>()
        }
        QuarkBudgetScope::Task(task_id) => {
            let task = store.find(task_id)?;
            if !is_quark_source(&task.source) {
                return Ok(Vec::new());
            }
            Ok(vec![member_from_task(0, Some(task_id), &task, true, None)])
        }
    }
}

pub(crate) fn connection_targets(
    store: &TaskStore,
    budget: u8,
) -> Result<Vec<QuarkConnectionTarget>, StoreError> {
    let mut scopes = BTreeSet::new();
    for task in store.list()? {
        if !is_quark_source(&task.source)
            || is_terminal(task.status)
            || task.status == TaskStatus::Paused
        {
            continue;
        }
        match store.cloud_directory_group_for_task(task.id)? {
            Some(group) if group.status != TaskStatus::Paused => {
                scopes.insert(QuarkBudgetScope::Directory(group.id));
            }
            Some(_) => {}
            None => {
                scopes.insert(QuarkBudgetScope::Task(task.id));
            }
        }
    }
    let mut targets = Vec::new();
    for scope in scopes {
        for (task_id, connections) in allocation_for_scope(store, scope, budget)? {
            let task = store.find(task_id)?;
            let Some(gid) = task.aria_gid else {
                continue;
            };
            targets.push(QuarkConnectionTarget {
                task_id,
                gid,
                connections,
            });
        }
    }
    Ok(targets)
}

pub(crate) fn pending_connection_changes(
    applied: &BTreeMap<Uuid, AppliedConnection>,
    targets: &[QuarkConnectionTarget],
) -> Vec<QuarkConnectionTarget> {
    targets
        .iter()
        .filter(|target| {
            applied.get(&target.task_id).is_none_or(|current| {
                current.gid != target.gid || current.connections != target.connections
            })
        })
        .cloned()
        .collect()
}

pub(crate) fn connection_options(connections: u8) -> Aria2Options {
    let connections = connections.max(1).to_string();
    Aria2Options::from([
        ("split".into(), connections.clone()),
        ("max-connection-per-server".into(), connections),
    ])
}

pub(crate) async fn apply_connection_target<C: QuarkConnectionOptionClient>(
    client: &C,
    store: &TaskStore,
    target: &QuarkConnectionTarget,
) -> bool {
    is_current_quark_gid(store, target)
        && client
            .change_connections(&target.gid, connection_options(target.connections))
            .await
            .is_ok()
        && is_current_quark_gid(store, target)
}

fn member_from_task(
    order: u32,
    task_id: Option<Uuid>,
    task: &DownloadTask,
    item_is_pending: bool,
    fallback_size: Option<u64>,
) -> QuarkBudgetMember {
    let terminal = is_terminal(task.status);
    let remaining_bytes = task
        .total_length
        .or(fallback_size)
        .map(|total| total.saturating_sub(task.completed_length));
    QuarkBudgetMember {
        order,
        task_id,
        has_current_gid: task.aria_gid.is_some(),
        reserves_connection: item_is_pending && !terminal,
        remaining_bytes,
    }
}

fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Recycled
    )
}

fn is_current_quark_gid(store: &TaskStore, target: &QuarkConnectionTarget) -> bool {
    store.find(target.task_id).is_ok_and(|task| {
        is_quark_source(&task.source)
            && !is_terminal(task.status)
            && task.status != TaskStatus::Paused
            && task.aria_gid.as_deref() == Some(target.gid.as_str())
    })
}

fn is_quark_source(source: &str) -> bool {
    source.starts_with("cloud://quark/") || source.starts_with("cloud-share://quark/")
}

#[cfg(test)]
mod tests {
    use super::{
        allocate_connections, allocation_for_scope, apply_connection_target, connection_options,
        initial_connections_for_task, pending_connection_changes, AppliedConnection,
        QuarkBudgetMember, QuarkBudgetScope, QuarkConnectionOptionClient, QuarkConnectionTarget,
        MIN_PARALLEL_REMAINING_BYTES,
    };
    use crate::{
        cloud::CloudProvider,
        models::{NewCloudDirectoryGroup, NewCloudDirectoryGroupItem},
        NewTask, TaskStatus, TaskStore,
    };
    use std::{collections::BTreeMap, sync::Mutex};
    use uuid::Uuid;

    fn id(value: u128) -> Uuid {
        Uuid::from_u128(value)
    }

    fn submitted(order: u32, value: u128, remaining_bytes: Option<u64>) -> QuarkBudgetMember {
        QuarkBudgetMember {
            order,
            task_id: Some(id(value)),
            has_current_gid: true,
            reserves_connection: true,
            remaining_bytes,
        }
    }

    fn pending(order: u32, remaining_bytes: Option<u64>) -> QuarkBudgetMember {
        QuarkBudgetMember {
            order,
            task_id: None,
            has_current_gid: false,
            reserves_connection: true,
            remaining_bytes,
        }
    }

    fn released(order: u32) -> QuarkBudgetMember {
        QuarkBudgetMember {
            order,
            task_id: None,
            has_current_gid: false,
            reserves_connection: false,
            remaining_bytes: None,
        }
    }

    #[test]
    fn reserves_one_connection_for_each_pending_file_before_using_ranges() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(1, 2, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(2, 3, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(3, 4, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                pending(4, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                pending(5, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
            ],
        );

        assert_eq!(
            allocation,
            BTreeMap::from([(id(1), 1), (id(2), 1), (id(3), 1), (id(4), 1)])
        );
    }

    #[test]
    fn distributes_extra_connections_in_stable_round_robin_order() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, Some(4 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(1, 2, Some(4 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(2, 3, Some(4 * MIN_PARALLEL_REMAINING_BYTES)),
            ],
        );

        assert_eq!(
            allocation,
            BTreeMap::from([(id(1), 2), (id(2), 1), (id(3), 1)])
        );
    }

    #[test]
    fn cycles_extra_connections_across_all_eligible_files() {
        let allocation = allocate_connections(
            8,
            vec![
                submitted(0, 1, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(1, 2, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
            ],
        );

        assert_eq!(allocation, BTreeMap::from([(id(1), 4), (id(2), 4)]));
    }

    #[test]
    fn gives_all_unreserved_budget_to_the_only_eligible_file() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                submitted(1, 2, Some(MIN_PARALLEL_REMAINING_BYTES - 1)),
            ],
        );

        assert_eq!(allocation, BTreeMap::from([(id(1), 3), (id(2), 1)]));
    }

    #[test]
    fn leaves_unknown_and_sub_mebibyte_files_at_one_connection() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, None),
                submitted(1, 2, Some(MIN_PARALLEL_REMAINING_BYTES - 1)),
            ],
        );

        assert_eq!(allocation, BTreeMap::from([(id(1), 1), (id(2), 1)]));
    }

    #[test]
    fn treats_exactly_one_mebibyte_as_eligible() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, Some(MIN_PARALLEL_REMAINING_BYTES)),
                submitted(1, 2, Some(MIN_PARALLEL_REMAINING_BYTES - 1)),
            ],
        );

        assert_eq!(allocation, BTreeMap::from([(id(1), 3), (id(2), 1)]));
    }

    #[test]
    fn ignores_completed_or_failed_members_when_reclaiming_budget() {
        let allocation = allocate_connections(
            4,
            vec![
                submitted(0, 1, Some(8 * MIN_PARALLEL_REMAINING_BYTES)),
                released(1),
                released(2),
                released(3),
            ],
        );

        assert_eq!(allocation, BTreeMap::from([(id(1), 4)]));
    }

    #[test]
    fn directory_snapshot_reserves_unlinked_quark_members_before_ranges() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "directory",
                    "album",
                    "C:/Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new(
                        "one",
                        &[] as &[&str],
                        "one.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "two",
                        &[] as &[&str],
                        "two.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "three",
                        &[] as &[&str],
                        "three.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                ],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let mut task = store.create(NewTask::direct("cloud://quark/one")).unwrap();
        task.status = TaskStatus::Downloading;
        task.aria_gid = Some("current-gid".into());
        task.total_length = Some(8 * MIN_PARALLEL_REMAINING_BYTES);
        let task = store.apply_transfer_snapshot(task).unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        let allocation =
            allocation_for_scope(&store, QuarkBudgetScope::Directory(group.id), 4).unwrap();

        assert_eq!(allocation, BTreeMap::from([(task.id, 2)]));
    }

    #[test]
    fn emits_a_change_once_and_reemits_after_a_gid_is_replaced() {
        let target = QuarkConnectionTarget {
            task_id: id(1),
            gid: "first-gid".into(),
            connections: 4,
        };
        let mut applied = BTreeMap::new();

        assert_eq!(
            pending_connection_changes(&applied, &[target.clone()]),
            vec![target.clone()]
        );

        applied.insert(
            target.task_id,
            AppliedConnection {
                gid: target.gid.clone(),
                connections: target.connections,
            },
        );
        assert!(pending_connection_changes(&applied, &[target.clone()]).is_empty());

        let replacement = QuarkConnectionTarget {
            gid: "replacement-gid".into(),
            ..target
        };
        assert_eq!(
            pending_connection_changes(&applied, &[replacement.clone()]),
            vec![replacement]
        );
    }

    #[test]
    fn initial_directory_submission_starts_at_one_before_its_gid_can_be_rebalanced() {
        let store = TaskStore::open_in_memory().unwrap();
        let group = store
            .create_cloud_directory_group(
                NewCloudDirectoryGroup::new(
                    CloudProvider::Quark,
                    "directory",
                    "album",
                    "C:/Downloads",
                ),
                &[
                    NewCloudDirectoryGroupItem::new(
                        "one",
                        &[] as &[&str],
                        "one.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "two",
                        &[] as &[&str],
                        "two.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                    NewCloudDirectoryGroupItem::new(
                        "three",
                        &[] as &[&str],
                        "three.bin",
                        Some(8 * MIN_PARALLEL_REMAINING_BYTES),
                    ),
                ],
            )
            .unwrap();
        let item = store
            .cloud_directory_group_items(group.id)
            .unwrap()
            .remove(0);
        let mut task = store.create(NewTask::direct("cloud://quark/one")).unwrap();
        task.total_length = Some(8 * MIN_PARALLEL_REMAINING_BYTES);
        let task = store.apply_transfer_snapshot(task).unwrap();
        store
            .link_cloud_directory_group_item_task(group.id, item.id, task.id)
            .unwrap();

        assert_eq!(initial_connections_for_task(&store, task.id, 4).unwrap(), 1);
        let mut submitted = store.find(task.id).unwrap();
        submitted.aria_gid = Some("submitted-gid".into());
        store.apply_transfer_snapshot(submitted).unwrap();
        assert_eq!(initial_connections_for_task(&store, task.id, 4).unwrap(), 2);
    }

    #[test]
    fn initial_standalone_submission_expands_only_when_size_is_known_and_large_enough() {
        let store = TaskStore::open_in_memory().unwrap();
        let task = store.create(NewTask::direct("cloud://quark/one")).unwrap();

        assert_eq!(initial_connections_for_task(&store, task.id, 4).unwrap(), 1);

        let mut known_large_task = store.find(task.id).unwrap();
        known_large_task.total_length = Some(8 * MIN_PARALLEL_REMAINING_BYTES);
        let known_large_task = store.apply_transfer_snapshot(known_large_task).unwrap();

        assert_eq!(
            initial_connections_for_task(&store, known_large_task.id, 4).unwrap(),
            4
        );
    }

    #[derive(Default)]
    struct RecordingChangeOptionClient {
        fail: bool,
        calls: Mutex<Vec<(String, BTreeMap<String, String>)>>,
    }

    #[async_trait::async_trait]
    impl QuarkConnectionOptionClient for RecordingChangeOptionClient {
        async fn change_connections(
            &self,
            gid: &str,
            options: BTreeMap<String, String>,
        ) -> Result<(), ()> {
            self.calls.lock().unwrap().push((gid.into(), options));
            (!self.fail).then_some(()).ok_or(())
        }
    }

    #[tokio::test]
    async fn changes_only_the_current_gid_with_the_two_connection_options() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut task = store.create(NewTask::direct("cloud://quark/one")).unwrap();
        task.status = TaskStatus::Downloading;
        task.aria_gid = Some("current-gid".into());
        let task = store.apply_transfer_snapshot(task).unwrap();
        let client = RecordingChangeOptionClient::default();
        let stale = QuarkConnectionTarget {
            task_id: task.id,
            gid: "stale-gid".into(),
            connections: 4,
        };
        let current = QuarkConnectionTarget {
            gid: "current-gid".into(),
            ..stale.clone()
        };

        assert!(!apply_connection_target(&client, &store, &stale).await);
        assert!(apply_connection_target(&client, &store, &current).await);
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            &[("current-gid".into(), connection_options(4))]
        );
        assert_eq!(
            store.find(task.id).unwrap().aria_gid.as_deref(),
            Some("current-gid")
        );
    }

    #[tokio::test]
    async fn failed_option_change_keeps_the_existing_download_untouched() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut task = store.create(NewTask::direct("cloud://quark/one")).unwrap();
        task.status = TaskStatus::Downloading;
        task.aria_gid = Some("current-gid".into());
        task.completed_length = 123;
        let task = store.apply_transfer_snapshot(task).unwrap();
        let client = RecordingChangeOptionClient {
            fail: true,
            ..Default::default()
        };
        let target = QuarkConnectionTarget {
            task_id: task.id,
            gid: "current-gid".into(),
            connections: 4,
        };

        assert!(!apply_connection_target(&client, &store, &target).await);
        let current = store.find(task.id).unwrap();
        assert_eq!(current.status, TaskStatus::Downloading);
        assert_eq!(current.aria_gid.as_deref(), Some("current-gid"));
        assert_eq!(current.completed_length, 123);
    }

    #[tokio::test]
    async fn does_not_expand_a_task_paused_after_target_selection() {
        let store = TaskStore::open_in_memory().unwrap();
        let mut task = store.create(NewTask::direct("cloud://quark/one")).unwrap();
        task.status = TaskStatus::Paused;
        task.aria_gid = Some("current-gid".into());
        let task = store.apply_transfer_snapshot(task).unwrap();
        let client = RecordingChangeOptionClient::default();
        let target = QuarkConnectionTarget {
            task_id: task.id,
            gid: "current-gid".into(),
            connections: 4,
        };

        assert!(!apply_connection_target(&client, &store, &target).await);
        assert!(client.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn retains_the_loop_when_activation_arrives_while_an_empty_plan_is_stopping() {
        let coordinator = super::QuarkDownloadCoordinator::default();
        coordinator
            .running
            .store(true, std::sync::atomic::Ordering::Release);
        coordinator
            .activation_generation
            .store(2, std::sync::atomic::Ordering::Release);

        assert!(coordinator.stop_or_reclaim_after_activation(1));
        assert!(coordinator
            .running
            .load(std::sync::atomic::Ordering::Acquire));
    }
}
