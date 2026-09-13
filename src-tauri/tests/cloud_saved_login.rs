use alldownloader_lib::cloud::{
    auth_supervisor::AuthSupervisor, commands::logout_provider, load_saved_password_login,
    prepare_password_account_login, run_with_session_recovery, save_password_credential,
    CloudPasswordCredential, CloudProvider, CredentialStore, MemoryCredentialStore, QuarkError,
    SessionWorkKind,
};
use std::cell::Cell;

const PROVIDERS: [CloudProvider; 3] = [
    CloudProvider::Baidu,
    CloudProvider::Tianyi,
    CloudProvider::Pikpak,
];

#[test]
fn logout_keeps_saved_login_for_each_password_provider() {
    for provider in PROVIDERS {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "a", "alice", "old-secret"),
        )
        .unwrap();
        store.save(provider.key(), b"session-a").unwrap();
        logout_provider(provider, &store).unwrap();
        assert!(store.load(provider.key()).unwrap().is_none());
        let saved = load_saved_password_login(&store, provider)
            .unwrap()
            .unwrap();
        assert_eq!(saved.username(), "alice");
        assert_eq!(saved.password(), "old-secret");
    }
}

#[test]
fn rejected_account_switch_keeps_old_login_until_a_successful_replacement() {
    for provider in PROVIDERS {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "a", "alice", "old-secret"),
        )
        .unwrap();
        store.save(provider.key(), b"session-a").unwrap();
        prepare_password_account_login(&store, provider, "bob").unwrap();
        assert!(store.load(provider.key()).unwrap().is_none());
        // A rejected login never reaches save_password_credential.
        let saved = load_saved_password_login(&store, provider)
            .unwrap()
            .unwrap();
        assert_eq!(saved.username(), "alice");
        assert_eq!(saved.password(), "old-secret");
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "b", "bob", "new-secret"),
        )
        .unwrap();
        let saved = load_saved_password_login(&store, provider)
            .unwrap()
            .unwrap();
        assert_eq!(saved.username(), "bob");
        assert_eq!(saved.password(), "new-secret");
    }
}

#[test]
fn manual_logout_blocks_automatic_password_recovery_until_explicit_login() {
    for provider in PROVIDERS {
        let store = MemoryCredentialStore::default();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "a", "alice", "secret"),
        )
        .unwrap();
        AuthSupervisor::default()
            .disconnect(&store, provider)
            .unwrap();
        // A new supervisor observes the durable state, as after restarting.
        let supervisor = AuthSupervisor::default();
        supervisor.restore(&store, provider).unwrap();
        struct NoRecovery;
        impl alldownloader_lib::cloud::driver::ProviderAuth for NoRecovery {
            fn refresh_session(&mut self) -> Result<bool, QuarkError> {
                panic!("manual logout must not refresh tokens or use a saved password");
            }
        }
        assert!(matches!(
            alldownloader_lib::cloud::auth_supervisor::recover_in_order(
                provider,
                &store,
                &mut NoRecovery,
                true,
                || panic!("must not capture a profile")
            ),
            Err(QuarkError::NotAuthorized)
        ));
        let relogins = Cell::new(0);
        let attempt = || {
            run_with_session_recovery(
                provider,
                &store,
                SessionWorkKind::Read,
                || {
                    if relogins.get() > 0 {
                        Ok(())
                    } else {
                        Err(QuarkError::NotAuthorized)
                    }
                },
                |_| {
                    relogins.set(relogins.get() + 1);
                    Ok(())
                },
            )
        };
        assert!(matches!(attempt(), Err(QuarkError::NotAuthorized)));
        assert_eq!(relogins.get(), 0);
        supervisor
            .ready(&store, provider, Some("a".into()), None, None)
            .unwrap();
        assert!(attempt().is_ok());
        assert_eq!(relogins.get(), 1);
    }
}

#[test]
fn manual_logout_survives_reopening_encrypted_credentials() {
    use alldownloader_lib::cloud::{
        auth_supervisor::is_manually_disconnected, DpapiCredentialStore,
    };
    let root = tempfile::tempdir().unwrap();
    let provider = CloudProvider::Tianyi;
    {
        let store = DpapiCredentialStore::new(root.path().to_owned()).unwrap();
        save_password_credential(
            &store,
            CloudPasswordCredential::new(provider, "a", "alice", "secret"),
        )
        .unwrap();
        logout_provider(provider, &store).unwrap();
        AuthSupervisor::default()
            .disconnect(&store, provider)
            .unwrap();
    }
    let reopened = DpapiCredentialStore::new(root.path().to_owned()).unwrap();
    assert!(is_manually_disconnected(&reopened, provider).unwrap());
    assert_eq!(
        load_saved_password_login(&reopened, provider)
            .unwrap()
            .unwrap()
            .username(),
        "alice"
    );
}
