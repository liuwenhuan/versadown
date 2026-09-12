use alldownloader_lib::cloud::{
    load_password_credential, remove_password_credential, run_with_session_recovery,
    save_password_credential, select_password_credential_account, CloudPasswordCredential,
    CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError, SessionWorkKind,
};
use std::{
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

#[test]
fn saved_credential_response_never_serializes_the_saved_password() {
    use alldownloader_lib::cloud::saved_cloud_credential_info;
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "stored-secret"),
    )
    .unwrap();
    let response = saved_cloud_credential_info(&store, CloudProvider::Baidu)
        .unwrap()
        .unwrap();
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"username": "alice", "hasSavedPassword": true})
    );
}

#[test]
fn replacing_browser_account_clears_previous_session_password_and_profile_owner() {
    use alldownloader_lib::cloud::{
        baidu_browser_profile_username, set_baidu_browser_profile_username,
    };
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "secret-a"),
    )
    .unwrap();
    store.save("baidu", b"session-a").unwrap();
    set_baidu_browser_profile_username(&store, Some("alice")).unwrap();
    select_password_credential_account(&store, CloudProvider::Baidu, "b").unwrap();
    assert!(store.load("baidu").unwrap().is_none());
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
    assert!(baidu_browser_profile_username(&store).unwrap().is_none());
}

#[test]
fn saved_password_handle_is_bound_to_username_and_rejects_ambiguous_inputs() {
    use alldownloader_lib::cloud::{resolve_password_login, PasswordLoginRequest};
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "saved-secret"),
    )
    .unwrap();
    let resolve = |username: &str, password: Option<&str>, use_saved_password| {
        resolve_password_login(
            &store,
            PasswordLoginRequest {
                provider: CloudProvider::Baidu,
                username: username.into(),
                password: password.map(str::to_owned),
                use_saved_password,
            },
        )
    };
    assert_eq!(
        resolve("alice", None, true).unwrap().1.as_str(),
        "saved-secret"
    );
    assert_eq!(
        resolve("alice", Some(""), true).unwrap().1.as_str(),
        "saved-secret"
    );
    assert_eq!(
        resolve("bob", Some("new-secret"), false)
            .unwrap()
            .1
            .as_str(),
        "new-secret"
    );
    assert!(resolve("bob", None, true).is_err());
    assert!(resolve("alice", Some("new-secret"), true).is_err());
    assert!(resolve("alice", None, false).is_err());
    assert!(resolve("alice", Some(""), false).is_err());
}

#[test]
fn replacement_removes_old_session_even_when_password_persistence_fails() {
    use std::sync::atomic::AtomicBool;
    struct FailingStore {
        inner: MemoryCredentialStore,
        fail_save: AtomicBool,
    }
    impl CredentialStore for FailingStore {
        fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
            if self.fail_save.load(Ordering::Relaxed) {
                return Err(QuarkError::Storage);
            }
            self.inner.save(key, bytes)
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
            self.inner.load(key)
        }
        fn remove(&self, key: &str) -> Result<(), QuarkError> {
            self.inner.remove(key)
        }
    }
    let store = FailingStore {
        inner: MemoryCredentialStore::default(),
        fail_save: AtomicBool::new(false),
    };
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "secret-a"),
    )
    .unwrap();
    store.save("baidu", b"session-a").unwrap();
    store.fail_save.store(true, Ordering::Relaxed);
    assert!(save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "b", "bob", "secret-b")
    )
    .is_err());
    assert!(store.load("baidu").unwrap().is_none());
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
}

#[test]
fn password_replacement_clears_all_old_owners_before_saving_the_new_draft() {
    use alldownloader_lib::cloud::{
        baidu_browser_profile_username, prepare_password_account_login, save_password_login_draft,
        saved_cloud_credential_info, set_baidu_browser_profile_username,
    };
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "a", "alice", "secret-a"),
    )
    .unwrap();
    store.save("baidu", b"session-a").unwrap();
    set_baidu_browser_profile_username(&store, Some("alice")).unwrap();
    assert!(prepare_password_account_login(&store, CloudProvider::Baidu, "bob").unwrap());
    assert!(store.load("baidu").unwrap().is_none());
    assert!(load_password_credential(&store, CloudProvider::Baidu)
        .unwrap()
        .is_none());
    assert!(baidu_browser_profile_username(&store).unwrap().is_none());
    save_password_login_draft(&store, CloudProvider::Baidu, "bob", "secret-b").unwrap();
    assert_eq!(
        saved_cloud_credential_info(&store, CloudProvider::Baidu)
            .unwrap()
            .unwrap()
            .username,
        "bob"
    );
}

#[test]
fn unreadable_old_credentials_abort_persistence_and_clear_the_session() {
    struct UnreadablePassword {
        inner: MemoryCredentialStore,
    }
    impl CredentialStore for UnreadablePassword {
        fn save(&self, key: &str, bytes: &[u8]) -> Result<(), QuarkError> {
            self.inner.save(key, bytes)
        }
        fn load(&self, key: &str) -> Result<Option<Vec<u8>>, QuarkError> {
            if key == "cloud-password-baidu" {
                return Err(QuarkError::Storage);
            }
            self.inner.load(key)
        }
        fn remove(&self, key: &str) -> Result<(), QuarkError> {
            self.inner.remove(key)
        }
    }
    let store = UnreadablePassword {
        inner: MemoryCredentialStore::default(),
    };
    store.save("baidu", b"session").unwrap();
    assert!(save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Baidu, "b", "bob", "secret-b")
    )
    .is_err());
    assert!(store.load("baidu").unwrap().is_none());
}

#[test]
fn replacing_pikpak_clears_credentials_without_requesting_an_unsupported_browser_profile() {
    use alldownloader_lib::cloud::prepare_password_account_login;
    let store = MemoryCredentialStore::default();
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Pikpak, "a", "alice", "secret-a"),
    )
    .unwrap();
    store.save("pikpak", b"session-a").unwrap();
    let reset_password_profile =
        prepare_password_account_login(&store, CloudProvider::Pikpak, "bob").unwrap();
    assert!(!reset_password_profile);
    assert!(store.load("pikpak").unwrap().is_none());
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_none());
}

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
fn fallback_password_is_redacted_replaced_by_a_new_account_and_removed_on_logout() {
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
    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Pikpak, "account-2", "user", "secret"),
    )
    .unwrap();
    let replacement = load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .expect("the latest account remains active");
    assert_eq!(replacement.account_id(), "account-2");

    select_password_credential_account(&store, CloudProvider::Pikpak, "account-1").unwrap();
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_none());

    save_password_credential(
        &store,
        CloudPasswordCredential::new(CloudProvider::Pikpak, "account-2", "user", "secret"),
    )
    .unwrap();
    remove_password_credential(&store, CloudProvider::Pikpak).unwrap();
    assert!(load_password_credential(&store, CloudProvider::Pikpak)
        .unwrap()
        .is_none());
}
