use crate::model::ProviderKind;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub trait CredentialStore: Send + Sync {
    fn get(&self, provider: ProviderKind, host: &str) -> Result<Option<String>>;
    fn set(&self, provider: ProviderKind, host: &str, secret: &str) -> Result<()>;
    fn remove(&self, provider: ProviderKind, host: &str) -> Result<()>;
}

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    values: Arc<RwLock<HashMap<String, String>>>,
}

pub struct KeyringCredentialStore;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthAccount {
    pub provider: ProviderKind,
    pub host: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Environment,
    Keyring,
    Gh,
    Anonymous,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCredential {
    secret: Option<String>,
    source: CredentialSource,
}

impl ResolvedCredential {
    pub fn secret(&self) -> Option<&str> {
        self.secret.as_deref()
    }

    pub fn source(&self) -> CredentialSource {
        self.source
    }
}

pub type GhFallback<'a> = Option<&'a dyn Fn() -> Result<Option<String>>>;

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, provider: ProviderKind, host: &str) -> Result<Option<String>> {
        let entry = keyring_entry(provider, host)?;
        match entry.get_password() {
            Ok(secret) if !secret.is_empty() => Ok(Some(secret)),
            Ok(_) => Ok(None),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(anyhow::anyhow!("keyring read failed: {error}")),
        }
    }

    fn set(&self, provider: ProviderKind, host: &str, secret: &str) -> Result<()> {
        if secret.is_empty() {
            bail!("credential cannot be empty");
        }
        keyring_entry(provider, host)?.set_password(secret)?;
        Ok(())
    }

    fn remove(&self, provider: ProviderKind, host: &str) -> Result<()> {
        match keyring_entry(provider, host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(anyhow::anyhow!("keyring removal failed: {error}")),
        }
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, provider: ProviderKind, host: &str) -> Result<Option<String>> {
        Ok(self
            .values
            .read()
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))?
            .get(&key(provider, host))
            .cloned())
    }

    fn set(&self, provider: ProviderKind, host: &str, secret: &str) -> Result<()> {
        if secret.is_empty() {
            bail!("credential cannot be empty");
        }
        self.values
            .write()
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))?
            .insert(key(provider, host), secret.into());
        Ok(())
    }

    fn remove(&self, provider: ProviderKind, host: &str) -> Result<()> {
        self.values
            .write()
            .map_err(|_| anyhow::anyhow!("credential store lock poisoned"))?
            .remove(&key(provider, host));
        Ok(())
    }
}

pub fn env_var_for_host(host: &str) -> String {
    let normalized: String = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PHROURION_TOKEN_{normalized}")
}

pub fn provider_env_var(provider: &ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::Github => Some("PHROURION_GITHUB_TOKEN"),
        ProviderKind::Gitlab => Some("PHROURION_GITLAB_TOKEN"),
        ProviderKind::Bitbucket | ProviderKind::BitbucketServer => {
            Some("PHROURION_BITBUCKET_TOKEN")
        }
        ProviderKind::Forgejo => None,
        ProviderKind::Local => None,
    }
}

pub fn resolve_credential(
    store: &dyn CredentialStore,
    provider: ProviderKind,
    host: &str,
    gh_fallback: GhFallback<'_>,
) -> Result<ResolvedCredential> {
    if let Some(name) = provider_env_var(&provider)
        && let Some(secret) = non_empty_env(name)
    {
        return Ok(ResolvedCredential {
            secret: Some(secret),
            source: CredentialSource::Environment,
        });
    }
    if let Some(secret) = non_empty_env(&env_var_for_host(host)) {
        return Ok(ResolvedCredential {
            secret: Some(secret),
            source: CredentialSource::Environment,
        });
    }
    if let Some(secret) = store.get(provider.clone(), host)? {
        return Ok(ResolvedCredential {
            secret: Some(secret),
            source: CredentialSource::Keyring,
        });
    }
    if provider == ProviderKind::Github
        && let Some(fallback) = gh_fallback
        && let Some(secret) = fallback()?
    {
        return Ok(ResolvedCredential {
            secret: Some(secret),
            source: CredentialSource::Gh,
        });
    }
    Ok(ResolvedCredential {
        secret: None,
        source: CredentialSource::Anonymous,
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn keyring_entry(provider: ProviderKind, host: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("phrourion", &key(provider, host))
        .context("cannot initialize the OS keyring")
}

fn key(provider: ProviderKind, host: &str) -> String {
    format!("{:?}:{}", provider, host.to_ascii_lowercase())
}
