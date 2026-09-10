use alldownloader_lib::cloud::{
    clear_password_credential_if_account_differs, load_password_credential,
    remove_password_credential, run_with_session_recovery, save_password_credential,
    CloudPasswordCredential, CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError,
    SessionWorkKind,
};
use std::{
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct LoadRecordingStore {
    load_calls: AtomicUsize,
}

impl CredentialStore for LoadRecordingStore {
    fn save(&self, _: &str, _: &[u8]) -> Result<(), QuarkError> {
        Ok(())
    }

    fn load(&self, _: &str) -> Result<Option<Vec<u8>>, QuarkError> {
        self.load_calls.fetch_add(1, Ordering::Relaxed);
        Err(QuarkError::Storage)
    }

    fn remove(&self, _: &str) -> Result<(), QuarkError> {
        Ok(())
    }
}

#[test]
fn expired_read_relogs_once_and_retries_once() {
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "account-1", "user", "secret"),
    )
    .unwrap();
    let work_calls = Cell::new(0);
    let relogin_calls = Cell::new(0);

    let result = run_with_session_recovery(
        CloudProvider::Baidu,
        &store,
        SessionWorkKind::Read,
        || {
            let call = work_calls.get() + 1;
            work_calls.set(call);
            if call == 1 {
                Err(QuarkError::AuthorizationExpired)
            } else {
                Ok("listed files")
            }
        },
        |credential| {
            relogin_calls.set(relogin_calls.get() + 1);
            assert_eq!(credential.account_id(), "account-1");
            assert_eq!(credential.username(), "user");
            assert_eq!(credential.password(), "secret");
            Ok(())
        },
    )
    .unwrap();

    assert_eq!(result, "listed files");
    assert_eq!(work_calls.get(), 2);
    assert_eq!(relogin_calls.get(), 1);
}

#[test]
fn expired_mutation_relogs_but_never_replays_work() {
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Tianyi, "account-1", "user", "secret"),
    )
    .unwrap();
    let work_calls = Cell::new(0);
    let relogin_calls = Cell::new(0);

    let result: Result<(), QuarkError> = run_with_session_recovery(
        CloudProvider::Tianyi,
        &store,
        SessionWorkKind::Mutation,
        || {
            work_calls.set(work_calls.get() + 1);
            Err(QuarkError::NotAuthorized)
        },
        |_| {
            relogin_calls.set(relogin_calls.get() + 1);
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(QuarkError::SessionRenewedRetryOperation)
    ));
    assert_eq!(work_calls.get(), 1);
    assert_eq!(relogin_calls.get(), 1);
}

#[test]
fn official_webview_provider_does_not_load_password_or_relogin_on_expired_session() {
    let store = LoadRecordingStore::default();
    let relogin_calls = Cell::new(0);

    let result: Result<(), QuarkError> = run_with_session_recovery(
        CloudProvider::Aliyun,
        &store,
        SessionWorkKind::Read,
        || Err(QuarkError::AuthorizationExpired),
        |_| {
            relogin_calls.set(relogin_calls.get() + 1);
            Ok(())
        },
    );

    assert!(matches!(result, Err(QuarkError::AuthorizationExpired)));
    assert_eq!(store.load_calls.load(Ordering::Relaxed), 0);
    assert_eq!(relogin_calls.get(), 0);
}

#[test]
fn fallback_password_is_redacted_and_removed_on_logout_or_account_switch() {
    let store = MemoryCredentialStore::default();
    let credential = CloudPasswordCredential::new(
        CloudProvider::Pikpak,
        "account-1",
        "name@example.com",
        "secret-value",
    );
    let debug = format!("{credential:?}");
    assert!(!debug.contains("name@example.com"));
    assert!(!debug.contains("secret-value"));
    save_password_credential(&store, credential).unwrap();

    clear_password_credential_if_account_differs(&store, CloudProvider::Pikpak, "account-1")
        .unwrap();
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_some());

    clear_password_credential_if_account_differs(&store, CloudProvider::Pikpak, "account-2")
        .unwrap();
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_none());

    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Pikpak, "account-1", "user", "secret"),
    )
    .unwrap();
    remove_password_credential(&store, CloudProvider::Pikpak).unwrap();
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_none());
}
