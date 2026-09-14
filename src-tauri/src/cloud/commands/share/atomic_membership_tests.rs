use super::*;
use crate::cloud::share::{CloudShareEntry, CloudSharePlan};
use uuid::Uuid;

#[tokio::test]
#[cfg(windows)]
async fn share_directory_invalid_item_never_leaves_an_independent_task() {
    let directory = tempfile::tempdir().unwrap();
    let plans = share::CloudSharePlanStore::new(
        crate::cloud::DpapiCredentialStore::new(directory.path().into()).unwrap(),
    );
    let plan = CloudSharePlan::new(
        CloudProvider::Baidu,
        "atomic-membership".into(),
        None,
        vec![CloudShareEntry::new(
            "atomic-membership",
            "file".into(),
            "file.txt".into(),
            vec![],
            Some(7),
        )],
        serde_json::json!({}),
    );
    let source = plan.source_for("file");
    plans.upsert(&plan).unwrap();
    let tasks = Arc::new(TaskStore::open_in_memory().unwrap());
    let result = submit_new_cloud_share_file_scoped_with(
        tasks.clone(),
        &plans,
        source,
        Default::default(),
        Some(DirectoryLeafContext {
            group_id: Uuid::new_v4(),
            item_id: Uuid::new_v4(),
        }),
        false,
        false,
        None,
        || async { panic!("invalid membership must fail before engine access") },
        || async { panic!("invalid membership must fail before provider access") },
    )
    .await;
    assert!(result.is_err());
    assert!(
        tasks.list().unwrap().is_empty(),
        "failed directory binding must not leave a top-level Parsing task"
    );
}
