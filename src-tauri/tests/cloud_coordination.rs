use alldownloader_lib::cloud::{
    commands::{restore_account, CloudState},
    *,
};
use std::{
    sync::{mpsc, Arc},
    time::Duration,
};

#[test]
fn logout_serializes_after_inflight_refresh_and_other_provider_remains_available() {
    let root = std::env::temp_dir().join(format!("cloud-coordination-{}", uuid::Uuid::new_v4()));
    let state = Arc::new(CloudState::new(root.clone()).unwrap());
    state
        .with_provider(CloudProvider::Pikpak, |s| s.save("pikpak", b"old"))
        .unwrap();
    let (refresh_started_tx, refresh_started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let refresh_state = state.clone();
    let refresh = std::thread::spawn(move || {
        refresh_state.with_provider(CloudProvider::Pikpak, |store| {
            assert!(store.load("pikpak")?.is_some());
            refresh_started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            store.save("pikpak", b"rotated")
        })
    });
    refresh_started_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let (other_tx, other_rx) = mpsc::channel();
    let other_state = state.clone();
    let other = std::thread::spawn(move || {
        let result = other_state.with_provider(CloudProvider::Quark, |store| store.load("quark"));
        other_tx.send(result).unwrap();
    });
    let other_result = other_rx.recv_timeout(Duration::from_secs(2));
    let (logout_tx, logout_rx) = mpsc::channel();
    let logout_state = state.clone();
    let logout = std::thread::spawn(move || {
        let result =
            logout_state.with_provider(CloudProvider::Pikpak, |store| store.remove("pikpak"));
        logout_tx.send(result).unwrap();
    });
    let premature_logout = logout_rx.recv_timeout(Duration::from_millis(80));
    release_tx.send(()).unwrap();
    refresh.join().unwrap().unwrap();
    logout.join().unwrap();
    other.join().unwrap();
    assert!(
        other_result.unwrap().unwrap().is_none(),
        "Quark must remain independently available"
    );
    assert!(
        premature_logout.is_err(),
        "logout must await earlier refresh"
    );
    assert!(
        state
            .with_provider(CloudProvider::Pikpak, |store| store.load("pikpak"))
            .unwrap()
            .is_none(),
        "refresh cannot resurrect a logged-out session"
    );
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn initial_account_restore_is_not_authorized_without_a_network_request() {
    struct NoNetwork;
    impl CloudHttpTransport for NoNetwork {
        fn send(&self, _: CloudHttpRequest) -> Result<CloudHttpResponse, QuarkError> {
            panic!("fresh install must not make account requests")
        }
    }
    let store = MemoryCredentialStore::default();
    for provider in [CloudProvider::Quark, CloudProvider::Pikpak] {
        let e = restore_account(provider, &store, &NoNetwork).err().unwrap();
        assert_eq!(serde_json::to_value(e).unwrap()["code"], "not_authorized");
    }
    assert!(matches!(
        PikPakClient::new(&NoNetwork, &store).list("0"),
        Err(QuarkError::AuthorizationExpired)
    ));
}
