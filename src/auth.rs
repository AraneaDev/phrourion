use crate::{
    command,
    model::ProviderKind,
    provider_http::{Auth, HttpClient},
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::Path,
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
    let mut normalized = String::new();
    for byte in host.bytes() {
        if byte.is_ascii_alphanumeric() {
            normalized.push(byte.to_ascii_uppercase() as char);
        } else {
            normalized.push_str(&format!("_{byte:02X}"));
        }
    }
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

pub fn remove_replaced_credential(
    store: &dyn CredentialStore,
    accounts: &[AuthAccount],
    provider: &ProviderKind,
    host: &str,
) -> Result<()> {
    if let Some(previous) = accounts
        .iter()
        .find(|account| account.host.eq_ignore_ascii_case(host))
        && &previous.provider != provider
    {
        store.remove(previous.provider.clone(), &previous.host)?;
    }
    Ok(())
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
    // A keyring backend error (locked, daemon unavailable, no secret
    // service on a headless host) is treated the same as no saved entry:
    // fall through the precedence chain rather than failing resolution
    // outright, matching the documented headless/env-var fallback path.
    if let Some(secret) = store.get(provider.clone(), host).unwrap_or(None) {
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

pub async fn test_connection(provider: ProviderKind, host: &str) -> Result<String> {
    let credential = resolve_credential(&KeyringCredentialStore, provider.clone(), host, None)?;
    let secret = match credential.secret() {
        Some(secret) => secret.to_string(),
        None if provider == ProviderKind::Github => gh_token(host)
            .await?
            .context("no saved, environment, or GitHub CLI credential is configured")?,
        None => bail!("no saved or environment credential is configured"),
    };
    let auth = match provider {
        ProviderKind::Forgejo => Auth::Token(secret),
        ProviderKind::Github
        | ProviderKind::Gitlab
        | ProviderKind::Bitbucket
        | ProviderKind::BitbucketServer => Auth::Bearer(secret),
        ProviderKind::Local => bail!("local repositories do not have provider credentials"),
    };
    let client = HttpClient::new(&base_url(&provider, host), auth)?;
    let path = match provider {
        ProviderKind::Github => "user",
        ProviderKind::Gitlab => "user",
        ProviderKind::Bitbucket | ProviderKind::BitbucketServer => "user",
        ProviderKind::Forgejo => "user",
        ProviderKind::Local => unreachable!(),
    };
    let value = client.get_json_value(path).await?;
    Ok(value["login"]
        .as_str()
        .or_else(|| value["username"].as_str())
        .or_else(|| value["login_name"].as_str())
        .or_else(|| value["nickname"].as_str())
        .unwrap_or("authenticated user")
        .into())
}

async fn gh_token(host: &str) -> Result<Option<String>> {
    let executable = std::env::var("PHROURION_GH").unwrap_or_else(|_| "gh".into());
    let output = command::run(
        &executable,
        &["auth", "token", "--hostname", host],
        Path::new("."),
    )
    .await;
    match output {
        Ok(token) if !token.trim().is_empty() => Ok(Some(token.trim().into())),
        Ok(_) => Ok(None),
        Err(_) => Ok(None),
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn base_url(provider: &ProviderKind, host: &str) -> String {
    match provider {
        ProviderKind::Github => std::env::var("PHROURION_GITHUB_BASE_URL").unwrap_or_else(|_| {
            if host == "github.com" {
                "https://api.github.com/".into()
            } else {
                format!("https://{host}/api/v3/")
            }
        }),
        ProviderKind::Gitlab => std::env::var("PHROURION_GITLAB_BASE_URL").unwrap_or_else(|_| {
            if host == "gitlab.com" {
                "https://gitlab.com/api/v4/".into()
            } else {
                format!("https://{host}/api/v4/")
            }
        }),
        ProviderKind::Bitbucket => {
            std::env::var("PHROURION_BITBUCKET_BASE_URL").unwrap_or_else(|_| {
                if host == "bitbucket.org" {
                    "https://api.bitbucket.org/2.0/".into()
                } else {
                    format!("https://{host}/2.0/")
                }
            })
        }
        ProviderKind::BitbucketServer => std::env::var("PHROURION_BITBUCKET_SERVER_BASE_URL")
            .unwrap_or_else(|_| format!("https://{host}/rest/api/1.0/")),
        ProviderKind::Forgejo => std::env::var("PHROURION_FORGEJO_TEST_URL")
            .map(|url| format!("{url}/api/v1/"))
            .unwrap_or_else(|_| format!("https://{host}/api/v1/")),
        ProviderKind::Local => String::new(),
    }
}

fn keyring_entry(provider: ProviderKind, host: &str) -> Result<keyring::Entry> {
    keyring::Entry::new("phrourion", &key(provider, host))
        .context("cannot initialize the OS keyring")
}

fn key(provider: ProviderKind, host: &str) -> String {
    format!("{:?}:{}", provider, host.to_ascii_lowercase())
}
