use phrourion::{auth, auth::CredentialStore, model::ProviderKind};
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn host_environment_name_normalizes_ports_and_punctuation() {
    assert_eq!(
        auth::env_var_for_host("Forge.Example.com:3000"),
        "PHROURION_TOKEN_FORGE_2EEXAMPLE_2ECOM_3A3000"
    );
}

#[test]
fn host_environment_names_do_not_collide_for_punctuation_variants() {
    assert_ne!(
        auth::env_var_for_host("a-b.com"),
        auth::env_var_for_host("a.b-com")
    );
}

#[test]
fn memory_store_round_trips_a_provider_host_secret() {
    let store = auth::MemoryCredentialStore::default();

    store
        .set(ProviderKind::Gitlab, "gitlab.example", "test-token")
        .unwrap();

    assert_eq!(
        store
            .get(ProviderKind::Gitlab, "gitlab.example")
            .unwrap()
            .as_deref(),
        Some("test-token")
    );
}

#[test]
fn environment_credentials_override_keyring_and_generic_host_values() {
    let _lock = ENV_LOCK.lock().unwrap();
    let store = auth::MemoryCredentialStore::default();
    store
        .set(ProviderKind::Gitlab, "auth.example", "keyring-token")
        .unwrap();
    let provider_var = "PHROURION_GITLAB_TOKEN";
    let host_var = auth::env_var_for_host("auth.example");
    unsafe {
        std::env::remove_var(provider_var);
        std::env::set_var(&host_var, "generic-token");
    }
    let generic =
        auth::resolve_credential(&store, ProviderKind::Gitlab, "auth.example", None).unwrap();
    assert_eq!(generic.secret(), Some("generic-token"));
    unsafe {
        std::env::set_var(provider_var, "provider-token");
    }
    let provider =
        auth::resolve_credential(&store, ProviderKind::Gitlab, "auth.example", None).unwrap();
    assert_eq!(provider.secret(), Some("provider-token"));
    unsafe {
        std::env::remove_var(provider_var);
        std::env::remove_var(&host_var);
    }
}

#[test]
fn github_uses_gh_fallback_after_environment_and_keyring() {
    let _lock = ENV_LOCK.lock().unwrap();
    let store = auth::MemoryCredentialStore::default();
    let host_var = auth::env_var_for_host("github.example");
    unsafe {
        std::env::remove_var("PHROURION_GITHUB_TOKEN");
        std::env::remove_var(&host_var);
    }
    let resolved = auth::resolve_credential(
        &store,
        ProviderKind::Github,
        "github.example",
        Some(&|| Ok(Some("gh-token".into()))),
    )
    .unwrap();
    assert_eq!(resolved.source(), auth::CredentialSource::Gh);
    assert_eq!(resolved.secret(), Some("gh-token"));
}

#[test]
fn replacing_a_host_account_removes_the_old_provider_secret() {
    let store = auth::MemoryCredentialStore::default();
    store
        .set(ProviderKind::Gitlab, "forge.example", "old-token")
        .unwrap();
    let accounts = vec![auth::AuthAccount {
        provider: ProviderKind::Gitlab,
        host: "forge.example".into(),
        label: String::new(),
        username: String::new(),
    }];
    auth::remove_replaced_credential(&store, &accounts, &ProviderKind::Forgejo, "FORGE.EXAMPLE")
        .unwrap();
    assert!(
        store
            .get(ProviderKind::Gitlab, "forge.example")
            .unwrap()
            .is_none()
    );
}
