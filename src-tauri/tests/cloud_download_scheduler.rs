use std::time::Duration;

use versadown_lib::cloud::{
    download_scheduler::{AdmissionOutcome, CloudDownloadScheduler},
    CloudProvider,
};
use tokio::{task::JoinHandle, time::timeout};
use uuid::Uuid;

async fn wait_until_pending(
    scheduler: &CloudDownloadScheduler,
    provider: CloudProvider,
    count: usize,
) {
    timeout(Duration::from_secs(2), async {
        while scheduler.pending(provider).len() != count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("waiter must register");
}

fn spawn_waiter(
    scheduler: &CloudDownloadScheduler,
    provider: CloudProvider,
    id: Uuid,
) -> JoinHandle<AdmissionOutcome> {
    let scheduler = scheduler.clone();
    tokio::spawn(async move { scheduler.enqueue(provider, id).await })
}

async fn fill_quark(scheduler: &CloudDownloadScheduler) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for _ in 0..4 {
        let id = Uuid::new_v4();
        assert_eq!(
            scheduler.enqueue(CloudProvider::Quark, id).await,
            AdmissionOutcome::Granted
        );
        ids.push(id);
    }
    ids
}

#[tokio::test]
async fn fifth_quark_waits_until_explicit_release_and_oldest_waiter_wins() {
    let scheduler = CloudDownloadScheduler::new();
    let active = fill_quark(&scheduler).await;
    let first_id = Uuid::new_v4();
    let first = spawn_waiter(&scheduler, CloudProvider::Quark, first_id);
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    let second_id = Uuid::new_v4();
    let second = spawn_waiter(&scheduler, CloudProvider::Quark, second_id);
    wait_until_pending(&scheduler, CloudProvider::Quark, 2).await;
    assert_eq!(
        scheduler.pending(CloudProvider::Quark),
        vec![first_id, second_id]
    );
    assert!(!first.is_finished());
    assert!(!second.is_finished());
    assert!(scheduler.release(active[0]));
    assert_eq!(
        timeout(Duration::from_secs(2), first)
            .await
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Granted
    );
    assert_eq!(scheduler.pending(CloudProvider::Quark), vec![second_id]);
    assert!(!second.is_finished());
    scheduler.release(first_id);
    assert_eq!(
        timeout(Duration::from_secs(2), second)
            .await
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Granted
    );
}

#[tokio::test]
async fn provider_slots_are_independent() {
    let scheduler = CloudDownloadScheduler::new();
    for _ in 0..4 {
        assert_eq!(
            scheduler
                .enqueue(CloudProvider::Xunlei, Uuid::new_v4())
                .await,
            AdmissionOutcome::Granted
        );
    }
    let waiter = spawn_waiter(&scheduler, CloudProvider::Xunlei, Uuid::new_v4());
    wait_until_pending(&scheduler, CloudProvider::Xunlei, 1).await;
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            scheduler.enqueue(CloudProvider::Baidu, Uuid::new_v4())
        )
        .await
        .unwrap(),
        AdmissionOutcome::Granted
    );
    assert!(!waiter.is_finished());
    waiter.abort();
}

#[tokio::test]
async fn cancelling_a_waiter_wakes_it_without_releasing_an_active_slot() {
    let scheduler = CloudDownloadScheduler::new();
    let active = fill_quark(&scheduler).await;
    let cancelled_id = Uuid::new_v4();
    let cancelled = spawn_waiter(&scheduler, CloudProvider::Quark, cancelled_id);
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    assert!(!scheduler.cancel(active[0]));
    assert!(scheduler.cancel(cancelled_id));
    assert_eq!(
        timeout(Duration::from_secs(2), cancelled)
            .await
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Cancelled
    );
    assert!(scheduler.pending(CloudProvider::Quark).is_empty());
    let next = spawn_waiter(&scheduler, CloudProvider::Quark, Uuid::new_v4());
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    assert!(!next.is_finished());
    scheduler.release(active[0]);
    assert_eq!(
        timeout(Duration::from_secs(2), next)
            .await
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Granted
    );
}

#[tokio::test]
async fn duplicate_ids_never_take_a_second_slot_or_queue_position() {
    let scheduler = CloudDownloadScheduler::new();
    let active = fill_quark(&scheduler).await;
    assert_eq!(
        scheduler.enqueue(CloudProvider::Baidu, active[0]).await,
        AdmissionOutcome::AlreadyTracked
    );
    let id = Uuid::new_v4();
    let waiter = spawn_waiter(&scheduler, CloudProvider::Quark, id);
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    assert_eq!(
        scheduler.enqueue(CloudProvider::Quark, id).await,
        AdmissionOutcome::AlreadyTracked
    );
    assert_eq!(scheduler.pending(CloudProvider::Quark), vec![id]);
    scheduler.cancel(id);
    assert_eq!(waiter.await.unwrap(), AdmissionOutcome::Cancelled);
}

#[tokio::test]
async fn aborting_an_enqueue_future_does_not_leave_a_phantom_fifo_head() {
    let scheduler = CloudDownloadScheduler::new();
    let active = fill_quark(&scheduler).await;
    let abandoned = spawn_waiter(&scheduler, CloudProvider::Quark, Uuid::new_v4());
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    abandoned.abort();
    assert!(abandoned.await.unwrap_err().is_cancelled());
    assert!(scheduler.pending(CloudProvider::Quark).is_empty());
    scheduler.release(active[0]);
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            scheduler.enqueue(CloudProvider::Quark, Uuid::new_v4())
        )
        .await
        .unwrap(),
        AdmissionOutcome::Granted
    );
}

#[tokio::test]
async fn directory_uses_one_provider_task_slot_but_admits_its_configured_file_parallelism() {
    let scheduler = CloudDownloadScheduler::new();
    let directory = Uuid::new_v4();
    let members: Vec<_> = (0..4).map(|_| Uuid::new_v4()).collect();

    for member in &members {
        assert_eq!(
            scheduler
                .enqueue_directory(CloudProvider::Quark, directory, *member, 4)
                .await,
            AdmissionOutcome::Granted
        );
    }

    // A fifth directory still fits: the first directory consumes one logical
    // provider task, rather than four file-task slots.
    assert_eq!(
        scheduler
            .enqueue_directory(CloudProvider::Quark, Uuid::new_v4(), Uuid::new_v4(), 4)
            .await,
        AdmissionOutcome::Granted
    );

    let fifth_member = Uuid::new_v4();
    let waiter = {
        let scheduler = scheduler.clone();
        tokio::spawn(async move {
            scheduler
                .enqueue_directory(CloudProvider::Quark, directory, fifth_member, 4)
                .await
        })
    };
    wait_until_pending(&scheduler, CloudProvider::Quark, 1).await;
    assert!(!waiter.is_finished());

    assert!(scheduler.release_directory_member(directory, members[0], false));
    assert_eq!(
        timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap(),
        AdmissionOutcome::Granted
    );
}
